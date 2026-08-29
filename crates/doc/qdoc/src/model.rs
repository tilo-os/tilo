//! Borrowed-view model operations over a `&mut AutoCommit`, so a document can live
//! in any host's automerge doc (e.g. `ui::State.doc`), not only an owned [`crate::Document`].

use crate::document::{DocError, MarkSpan};
use crate::schema::{
    ATTR_BLOB_BYTES, ATTR_BLOB_H, ATTR_BLOB_MIME, ATTR_BLOB_W, BLOB_H, BLOB_LEN, BLOB_MIME, BLOB_W,
    BLOCK_ATTRS, BLOCK_CHILDREN, BLOCK_KIND, BLOCK_ORDER, BLOCK_TEXT, CHANGE_AUTHOR, CHANGE_DATE,
    CHANGE_KIND, COMMENT_AUTHOR, COMMENT_BLOCKS, COMMENT_DATE, CROSS_REF_PREFIX, DEFAULT_COLUMNS,
    FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, HEADING_REF_PREFIX, KEY_BLOBS, KEY_BLOCKS, KEY_CHANGES,
    KEY_COMMENTS, KEY_FOOTER, KEY_HEADER, KEY_META, KEY_NOTES, KEY_PRESERVED, KEY_PROPS,
    KEY_SCHEMA, KEY_SCHEMA_VERSION, KEY_STYLES, MARK_BOOKMARK, MARK_COMMENT, MARK_DEL, MARK_FIELD,
    MARK_INS, MARK_LINK, MARK_NOTE_REF, META_COLUMN_GAP, META_COLUMNS, META_LANGUAGE, NOTE_BLOCKS,
    NOTE_KIND, NOTE_KIND_ENDNOTE, NOTE_KIND_FOOTNOTE, PRESERVED_SEG_BYTES, PRESERVED_SEG_ORD,
    SCHEMA_ID, SCHEMA_VERSION,
};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

/// Resolved object ids for the canonical container maps under a document root.
#[derive(Clone, Debug)]
pub struct Containers {
    /// The document root these containers live under (not always `automerge::ROOT` — a host may nest the model); needed to lazily create the blobs map.
    pub root: ObjId,
    pub blocks: ObjId,
    pub styles: ObjId,
    pub preserved: ObjId,
    pub meta: ObjId,
    /// Content-addressed binary descriptors (blake3 hex → descriptor; bytes live outside the CRDT, see [`crate::blobstore`]).
    /// `None` for documents predating the blobs map; created lazily on first [`DocMut::put_blob`].
    pub blobs: Option<ObjId>,
    /// Page-header block container (same shape as `blocks`); `None` only for documents predating the region, created lazily on first header write.
    pub header: Option<ObjId>,
    /// Page-footer block container (same shape as `blocks`), mirroring `header`.
    pub footer: Option<ObjId>,
    /// Footnote/endnote container (`Map<note_id, note>`); `None` for documents predating notes, created lazily on first insert.
    pub notes: Option<ObjId>,
    /// Comment/annotation container (`Map<comment_id, comment>`). Eager-initialised (unlike `header`/`notes`) so concurrent comment adds converge on one shared object;
    /// `None` only for documents predating the comments map.
    pub comments: Option<ObjId>,
    /// Tracked-changes container (`Map<change_id, change>`). Eager-initialised like `comments`; `None` only for documents predating the changes map.
    pub changes: Option<ObjId>,
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
    pub fn from_str(s: &str) -> Self {
        if s == NOTE_KIND_ENDNOTE {
            NoteKind::Endnote
        } else {
            NoteKind::Footnote
        }
    }
}

/// A block's `(order_key, id)` pair, ordered exactly as [`ordered_keys`] orders blocks
/// (fractional key via [`crate::order::cmp_keys`], then id as a tiebreak). Used as the in-memory order index's key type.
#[derive(Clone, PartialEq, Eq)]
struct OrderKey {
    key: Vec<u8>,
    id: String,
}

impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> Ordering {
        crate::order::cmp_keys(&self.key, &other.key).then_with(|| self.id.cmp(&other.id))
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
    by_id: HashMap<String, Vec<u8>>,
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

/// Per-replica, collision-free block id generator: `{actor_hex}.{counter}`.
pub struct IdGen {
    actor_hex: String,
    next: u64,
    /// In-memory block-order index (see [`OrderIndex`]); private bookkeeping carried on `IdGen` since it threads through every `DocMut`.
    order: OrderIndex,
}

impl IdGen {
    pub fn new(actor_hex: String) -> Self {
        Self {
            actor_hex,
            next: 0,
            order: OrderIndex::default(),
        }
    }
    /// Start the counter at `next` — used to seed past ids that already exist in a loaded/opened
    /// document (otherwise a fresh generator re-mints existing ids and overwrites those blocks).
    pub fn with_counter(actor_hex: String, next: u64) -> Self {
        Self {
            actor_hex,
            next,
            order: OrderIndex::default(),
        }
    }
    pub fn mint(&mut self) -> String {
        let id = format!("{}.{}", self.actor_hex, self.next);
        self.next += 1;
        id
    }
}

/// The next free id counter for `actor_hex`, scanning the existing block ids so a
/// generator created over an already-populated document won't collide.
pub fn next_counter_for(doc: &AutoCommit, c: &Containers, actor_hex: &str) -> u64 {
    let prefix = format!("{actor_hex}.");
    let mut next = 0u64;
    let mut bump = |id: &str| {
        if let Some(rest) = id.strip_prefix(&prefix)
            && let Ok(n) = rest.parse::<u64>()
        {
            next = next.max(n + 1);
        }
    };
    for id in blocks_in_order(doc, c) {
        bump(&id);
    }
    // Header/footer blocks, note ids, and note-body blocks draw from the same per-replica
    // generator, so a seeded generator must scan them too or it could re-mint an existing one.
    for region in [Region::Header, Region::Footer] {
        for id in region_blocks_in_order(doc, c, region) {
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
    for cid in comment_ids(doc, c) {
        bump(&cid);
        for bid in comment_blocks_in_order(doc, c, &cid) {
            bump(&bid);
        }
    }
    // Tracked-change ids draw from the same generator too, for the same reason.
    for chid in change_ids(doc, c) {
        bump(&chid);
    }
    next
}

/// An [`IdGen`] for `doc`'s current actor, seeded past existing block ids.
pub fn seeded_id_gen(doc: &AutoCommit, c: &Containers) -> IdGen {
    let actor = doc.get_actor().to_hex_string();
    let next = next_counter_for(doc, c, &actor);
    IdGen::with_counter(actor, next)
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
/// At the document root the schema is *adopted* from the shared [`genesis`]
/// rather than minted, so two replicas that each open the same file are editing
/// one document and not two that merge lossily (see [`GENESIS_ACTOR`]).
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
        if doc.load_incremental(genesis()).is_ok()
            && let Ok(adopted) = open(doc, root)
        {
            return adopted;
        }
        // Falling back to a private schema would reintroduce exactly the
        // silent-loss conflict the genesis exists to prevent, so say so.
        tracing::error!(
            "qdoc: could not adopt the shared schema genesis; this document's \
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
    // the genesis exists to prevent. Documents authored before a container
    // existed still open: it reads as `None` and is created on first write.
    // Present-and-empty is indistinguishable from absent to every reader, and
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
        blobs: Some(blobs),
        header: Some(header),
        footer: Some(footer),
        notes: Some(notes),
        comments: Some(comments),
        changes: Some(changes),
    }
}

/// Whether `actor` wrote the schema rather than any of the document's content.
///
/// Nobody edits as the genesis actor, so a change it authored is not a revision:
/// a version history that lists it attributes one to a person who does not exist.
pub fn is_schema_genesis_actor(actor: &automerge::ActorId) -> bool {
    actor.to_bytes() == GENESIS_ACTOR
}

/// The schema genesis actor as hex, for comparing against a recorded
/// `actor_hex`. See [`is_schema_genesis_actor`].
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

/// Resolve existing containers under `root`. The blobs map is optional so
/// documents written before it existed still open (it is then created lazily).
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<Containers, DocError> {
    Ok(Containers {
        root: root.clone(),
        blocks: map_at(doc, root, KEY_BLOCKS)?,
        styles: map_at(doc, root, KEY_STYLES)?,
        preserved: map_at(doc, root, KEY_PRESERVED)?,
        meta: map_at(doc, root, KEY_META)?,
        blobs: opt_map_at(doc, root, KEY_BLOBS),
        header: opt_map_at(doc, root, KEY_HEADER),
        footer: opt_map_at(doc, root, KEY_FOOTER),
        notes: opt_map_at(doc, root, KEY_NOTES),
        comments: opt_map_at(doc, root, KEY_COMMENTS),
        changes: opt_map_at(doc, root, KEY_CHANGES),
    })
}

fn map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Result<ObjId, DocError> {
    match doc.get(root, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}

/// Resolve an optional map (returns `None` if absent rather than erroring).
fn opt_map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(root, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Resolve the blobs map object, preferring the cached id and falling back to a
/// fresh root lookup (so a lazily-created map is still found via reads).
fn blobs_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    if let Some(b) = &c.blobs {
        return Some(b.clone());
    }
    opt_map_at(doc, &c.root, KEY_BLOBS)
}

/// Resolve a region's block container object, preferring the cached id and falling back to a fresh
/// root lookup. The body container always exists; header/footer may be `None` until first written.
fn region_obj(doc: &AutoCommit, c: &Containers, region: Region) -> Option<ObjId> {
    match region {
        Region::Body => Some(c.blocks.clone()),
        Region::Header => c
            .header
            .clone()
            .or_else(|| opt_map_at(doc, &c.root, KEY_HEADER)),
        Region::Footer => c
            .footer
            .clone()
            .or_else(|| opt_map_at(doc, &c.root, KEY_FOOTER)),
    }
}

/// The ordered block ids of a region (empty when the region is absent).
pub fn region_blocks_in_order(doc: &AutoCommit, c: &Containers, region: Region) -> Vec<String> {
    match region_obj(doc, c, region) {
        Some(obj) => ordered_keys(doc, &obj),
        None => Vec::new(),
    }
}

/// Materialize a region's blocks into [`BlockView`]s (empty when absent).
pub fn materialize_region(doc: &AutoCommit, c: &Containers, region: Region) -> Vec<BlockView> {
    match region_obj(doc, c, region) {
        Some(obj) => ordered_keys(doc, &obj)
            .into_iter()
            .map(|id| materialize_block(doc, &obj, &id))
            .collect(),
        None => Vec::new(),
    }
}

/// Resolve the notes container object, preferring the cached id and falling back to a fresh root lookup.
fn notes_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.notes
        .clone()
        .or_else(|| opt_map_at(doc, &c.root, KEY_NOTES))
}

/// Resolve a single note's sub-map object by id.
fn note_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let notes = notes_obj(doc, c)?;
    match doc.get(&notes, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

/// Resolve a note's block container (`note.blocks`) sub-map object.
fn note_blocks_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let note = note_obj(doc, c, id)?;
    match doc.get(&note, NOTE_BLOCKS) {
        Ok(Some((Value::Object(ObjType::Map), bid))) => Some(bid),
        _ => None,
    }
}

/// The note ids present in the document, in a stable (sorted) order so two replicas converge
/// byte-identically; citation order in the body text is recovered separately from `note-ref` marks.
pub fn note_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match notes_obj(doc, c) {
        Some(notes) => {
            let mut ids: Vec<String> = doc
                .keys(&notes)
                .filter(|id| {
                    matches!(
                        doc.get(&notes, id),
                        Ok(Some((Value::Object(ObjType::Map), _)))
                    )
                })
                .collect();
            ids.sort();
            ids
        }
        None => Vec::new(),
    }
}

/// A single note's kind (`Footnote`/`Endnote`), or `None` if the id is unknown.
pub fn note_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<NoteKind> {
    let note = note_obj(doc, c, id)?;
    match doc.get(&note, NOTE_KIND) {
        Ok(Some((v, _))) => v.to_str().map(NoteKind::from_str),
        _ => None,
    }
}

/// Materialize a note's block sequence (empty if the note has no blocks / is unknown).
pub fn materialize_note_blocks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<BlockView> {
    match note_blocks_obj(doc, c, id) {
        Some(obj) => ordered_keys(doc, &obj)
            .into_iter()
            .map(|bid| materialize_block(doc, &obj, &bid))
            .collect(),
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

/// Resolve the comments container object, preferring the cached id and falling back to a fresh root lookup.
fn comments_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.comments
        .clone()
        .or_else(|| opt_map_at(doc, &c.root, KEY_COMMENTS))
}

/// Resolve a single comment's sub-map object by id.
fn comment_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let comments = comments_obj(doc, c)?;
    match doc.get(&comments, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

/// Resolve a comment's block container (`comment.blocks`) sub-map object.
fn comment_blocks_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let comment = comment_obj(doc, c, id)?;
    match doc.get(&comment, COMMENT_BLOCKS) {
        Ok(Some((Value::Object(ObjType::Map), bid))) => Some(bid),
        _ => None,
    }
}

/// The comment ids present in the document, in a stable (sorted) order so two replicas that merged
/// in different orders materialize byte-identically; anchored ranges are recovered from the `comment` marks.
pub fn comment_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match comments_obj(doc, c) {
        Some(comments) => {
            let mut ids: Vec<String> = doc
                .keys(&comments)
                .filter(|id| {
                    matches!(
                        doc.get(&comments, id),
                        Ok(Some((Value::Object(ObjType::Map), _)))
                    )
                })
                .collect();
            ids.sort();
            ids
        }
        None => Vec::new(),
    }
}

/// A comment's author display name (empty if unset / unknown id).
pub fn comment_author(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let comment = comment_obj(doc, c, id)?;
    match doc.get(&comment, COMMENT_AUTHOR) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// A comment's date / timestamp string (empty if unset / unknown id).
pub fn comment_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let comment = comment_obj(doc, c, id)?;
    match doc.get(&comment, COMMENT_DATE) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
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
        Some(obj) => ordered_keys(doc, &obj)
            .into_iter()
            .map(|bid| materialize_block(doc, &obj, &bid))
            .collect(),
        None => Vec::new(),
    }
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
    comment_ids(doc, c)
        .into_iter()
        .map(|id| crate::snapshot::CommentView {
            author: comment_author(doc, c, &id).unwrap_or_default(),
            date: comment_date(doc, c, &id).unwrap_or_default(),
            blocks: materialize_comment_blocks(doc, c, &id),
            range: comment_range(doc, c, &id),
            id,
        })
        .collect()
}

/// Every bookmark in the body, recovered from the `bookmark` marks. Sorted by `(name, block, start)`
/// so replicas that merged in different orders materialize byte-identically; a name may repeat if the source document duplicated it.
pub fn bookmarks(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::BookmarkView> {
    let mut out: Vec<crate::snapshot::BookmarkView> = Vec::new();
    for bid in blocks_in_order(doc, c) {
        for m in marks(doc, c, &bid) {
            if m.name == MARK_BOOKMARK
                && let crate::document::MarkValue::Str(name) = &m.value
            {
                out.push(crate::snapshot::BookmarkView {
                    name: name.clone(),
                    range: (bid.clone(), m.start, m.end),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.name, &a.range.0, a.range.1, a.range.2)
            .cmp(&(&b.name, &b.range.0, b.range.1, b.range.2))
    });
    out
}

/// The auto/derived reference id of a heading block (so a cross-reference can target a heading with no
/// explicit bookmark): [`HEADING_REF_PREFIX`] + the block id. Stable across save/load, distinct from any user bookmark name.
pub fn heading_ref_id(block_id: &str) -> String {
    format!("{HEADING_REF_PREFIX}{block_id}")
}

/// Resolve the changes container object, preferring the cached id and falling back to a fresh root lookup.
fn changes_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.changes
        .clone()
        .or_else(|| opt_map_at(doc, &c.root, KEY_CHANGES))
}

/// Resolve a single change's sub-map object by id.
fn change_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let changes = changes_obj(doc, c)?;
    match doc.get(&changes, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

/// The change ids present in the document, in a stable (sorted) order so replicas converge byte-identically;
/// anchored ranges are recovered separately from the body `ins`/`del` marks.
pub fn change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match changes_obj(doc, c) {
        Some(changes) => {
            let mut ids: Vec<String> = doc
                .keys(&changes)
                .filter(|id| {
                    matches!(
                        doc.get(&changes, id),
                        Ok(Some((Value::Object(ObjType::Map), _)))
                    )
                })
                .collect();
            ids.sort();
            ids
        }
        None => Vec::new(),
    }
}

/// A change's author display name (empty if unset / unknown id).
pub fn change_author(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let change = change_obj(doc, c, id)?;
    match doc.get(&change, CHANGE_AUTHOR) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// A change's date / timestamp string (empty if unset / unknown id).
pub fn change_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let change = change_obj(doc, c, id)?;
    match doc.get(&change, CHANGE_DATE) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// A change's kind (`Ins`/`Del`), or `None` if the id is unknown.
pub fn change_kind(
    doc: &AutoCommit,
    c: &Containers,
    id: &str,
) -> Option<crate::snapshot::ChangeKind> {
    let change = change_obj(doc, c, id)?;
    match doc.get(&change, CHANGE_KIND) {
        Ok(Some((v, _))) => v.to_str().map(crate::snapshot::ChangeKind::from_str),
        _ => None,
    }
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
    change_ids(doc, c)
        .into_iter()
        .map(|id| {
            let kind = change_kind(doc, c, &id).unwrap_or(crate::snapshot::ChangeKind::Ins);
            crate::snapshot::ChangeView {
                kind,
                author: change_author(doc, c, &id).unwrap_or_default(),
                date: change_date(doc, c, &id).unwrap_or_default(),
                range: change_range(doc, c, &id),
                id,
            }
        })
        .collect()
}

/// A blob's descriptor — what the document holds *instead of* its bytes.
/// `None` when the document doesn't reference that hash at all, distinct from referencing it without having the content yet.
pub fn blob_meta(
    doc: &AutoCommit,
    c: &Containers,
    hash: &str,
) -> Option<crate::blobstore::BlobMeta> {
    let blobs = blobs_obj(doc, c)?;
    match doc.get(&blobs, hash) {
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
                    ScalarValue::Uint(v) => *v as i64,
                    _ => 0,
                },
                _ => 0,
            };
            Some(crate::blobstore::BlobMeta {
                mime: str_at(BLOB_MIME),
                len: int_at(BLOB_LEN).max(0) as u64,
                w: int_at(BLOB_W).max(0) as u32,
                h: int_at(BLOB_H).max(0) as u32,
            })
        }
        // A document authored before the bytes moved out still describes itself inline (length + format known).
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(crate::blobstore::sniff(b)),
            _ => None,
        },
        _ => None,
    }
}

/// Every blob hash this document references, whether or not the content is here yet.
pub fn blob_hashes(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match blobs_obj(doc, c) {
        Some(b) => doc.keys(&b).collect(),
        None => Vec::new(),
    }
}

/// Fetch a content-addressed blob's bytes by its blake3 hex hash, through the process-wide blob store.
/// `None` means the content hasn't arrived — a *knowable* state a renderer turns into a placeholder rather than nothing.
pub fn blob(doc: &AutoCommit, c: &Containers, hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = crate::blobstore::get(hash) {
        return Some(bytes);
    }
    // Documents written before the bytes moved out of the CRDT still carry them inline; serve those,
    // and warm the store so every later read of this hash takes the modern path.
    let blobs = blobs_obj(doc, c)?;
    match doc.get(&blobs, hash) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => {
                crate::blobstore::store().put(hash, b);
                Some(b.clone())
            }
            _ => None,
        },
        _ => None,
    }
}

/// The blobs whose content is available here, keyed by blake3 hex.
/// A referenced hash **absent** from this map means the content hasn't arrived yet — that absence is
/// load-bearing: it's what a renderer keys a placeholder off, and [`blob_meta`] sizes that placeholder.
pub fn blobs(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for k in blob_hashes(doc, c) {
        if let Some(bytes) = blob(doc, c, &k) {
            out.insert(k, bytes);
        }
    }
    out
}

fn block_obj(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(container, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

fn block_order(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<Vec<u8>> {
    let obj = block_obj(doc, container, id)?;
    match doc.get(&obj, BLOCK_ORDER) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

thread_local! {
    /// Per-thread count of full [`ordered_keys`] container scans (each an O(n) read-and-sort). The in-memory
    /// order index means a *bulk* insert (build/import of n blocks) triggers only O(1) of these — one per
    /// container to seed the index — instead of one per insert. The perf test asserts on this delta directly,
    /// so a regression back to per-insert scanning is caught mode-independently (no wall-clock dependence).
    static ORDERED_KEYS_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The number of full container scans this thread has performed (see [`ORDERED_KEYS_SCANS`]). Sample it
/// around a bulk insert to prove the order index kept work near-constant rather than O(n).
pub fn ordered_keys_scan_count() -> u64 {
    ORDERED_KEYS_SCANS.with(std::cell::Cell::get)
}

fn ordered_keys(doc: &AutoCommit, container: &ObjId) -> Vec<String> {
    ORDERED_KEYS_SCANS.with(|c| c.set(c.get() + 1));
    let mut v: Vec<(Vec<u8>, String)> = doc
        .keys(container)
        .filter_map(|id| block_order(doc, container, &id).map(|ord| (ord, id)))
        .collect();
    v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, id)| id).collect()
}

pub fn blocks_in_order(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    ordered_keys(doc, &c.blocks)
}

/// The byte length of every body block's fractional order key. The truncating [`crate::order::key_between`]
/// keeps these bounded for a run of sequential appends; a naive append-a-byte scheme grows them O(n) per key.
/// The perf test asserts the max stays small — a direct, mode-independent guard on the key-growth half of the fix.
pub fn block_order_key_lengths(doc: &AutoCommit, c: &Containers) -> Vec<usize> {
    doc.keys(&c.blocks)
        .filter_map(|id| block_order(doc, &c.blocks, &id).map(|k| k.len()))
        .collect()
}

/// Read a document-level meta string (e.g. the serialized stylesheet).
pub fn meta_str(doc: &AutoCommit, c: &Containers, key: &str) -> Option<String> {
    match doc.get(&c.meta, key) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// The document-wide multi-column layout: `(column_count, optional_gap_px)`. Reads the `meta.columns` /
/// `meta.column_gap` registers, defaulting to a single column ([`DEFAULT_COLUMNS`]) with no gap when unset
/// so documents predating this setting read as single-column. A stored count of zero is clamped up to one.
pub fn columns(doc: &AutoCommit, c: &Containers) -> (u32, Option<f64>) {
    let count = match doc.get(&c.meta, META_COLUMNS) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Uint(u) => *u as u32,
            ScalarValue::Int(i) if *i > 0 => *i as u32,
            _ => DEFAULT_COLUMNS,
        },
        _ => DEFAULT_COLUMNS,
    }
    .max(1);
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
    match doc.get(&c.meta, META_LANGUAGE) {
        Ok(Some((v, _))) => v
            .to_str()
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string),
        _ => None,
    }
}

/// Resolve the document-properties sub-map under `meta` (`meta.props`), if it exists (created lazily on first [`DocMut::set_doc_prop`]).
fn props_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    match doc.get(&c.meta, KEY_PROPS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Read a single document property (`title`/`author`/… — see [`DOC_PROP_KEYS`]), or any other string property name a codec stashed. `None` if unset.
pub fn doc_prop(doc: &AutoCommit, c: &Containers, name: &str) -> Option<String> {
    let props = props_obj(doc, c)?;
    match doc.get(&props, name) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// All document properties (`name → value`) as a sorted map, empty if none are set.
pub fn doc_props(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(props) = props_obj(doc, c) {
        for k in doc.keys(&props) {
            if let Ok(Some((v, _))) = doc.get(&props, &k)
                && let Some(s) = v.to_str()
            {
                out.insert(k, s.to_string());
            }
        }
    }
    out
}

pub fn block_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    match doc.get(&obj, BLOCK_KIND) {
        Ok(Some((v, _))) => v.to_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

pub fn block_text(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    match doc.get(&obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
        _ => None,
    }
}

fn marks_on(doc: &AutoCommit, block_obj: &ObjId) -> Vec<MarkSpan> {
    let tid = match doc.get(block_obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), t))) => t,
        _ => return vec![],
    };
    let mut spans: Vec<MarkSpan> = doc
        .marks(&tid)
        .map(|ms| {
            ms.into_iter()
                .map(|m| MarkSpan {
                    start: m.start,
                    end: m.end,
                    name: m.name().to_string(),
                    value: m.value().into(),
                })
                .collect()
        })
        .unwrap_or_default();
    // Canonical order so replicas that merged in different orders produce byte-identical materializations
    // (mark *set* is order-independent); value is part of the key so concurrent valued marks sort deterministically.
    spans.sort_by(|a, b| {
        (a.start, a.end, &a.name, &a.value).cmp(&(b.start, b.end, &b.name, &b.value))
    });
    spans
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
/// doesn't model), as `(key, bytes)`. Reconstructed exactly from however the part is stored — the legacy
/// single-`Bytes` scalar form, or the per-atom segmented `Map` form (see [`PRESERVED_SEGMENTS`]) — by concatenating segments in stored order.
pub fn preserved_parts(doc: &AutoCommit, c: &Containers) -> Vec<(String, Vec<u8>)> {
    doc.keys(&c.preserved)
        .filter_map(|k| preserved_part(doc, c, &k).map(|b| (k, b)))
        .collect()
}

/// Reconstruct one preserved part's verbatim bytes by key, transparently handling both the legacy
/// `Bytes`-scalar form and the per-atom segmented `Map` form (concatenating ordered segments).
pub fn preserved_part(doc: &AutoCommit, c: &Containers, key: &str) -> Option<Vec<u8>> {
    match doc.get(&c.preserved, key) {
        // Per-atom segmented form: `Map<segment_id, {ord: Bytes, bytes: Bytes}>`, sorted by `ord` then concatenated.
        // CRDT-merge-friendly: each segment is an independent automerge object, so edits to different atoms of the same part merge.
        Ok(Some((Value::Object(ObjType::Map), part_obj))) => {
            let mut segs: Vec<(Vec<u8>, String, Vec<u8>)> = doc
                .keys(&part_obj)
                .filter_map(|sid| {
                    let seg_obj = match doc.get(&part_obj, &sid) {
                        Ok(Some((Value::Object(ObjType::Map), o))) => o,
                        _ => return None,
                    };
                    let ord = match doc.get(&seg_obj, PRESERVED_SEG_ORD) {
                        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                            ScalarValue::Bytes(b) => b.clone(),
                            _ => Vec::new(),
                        },
                        _ => Vec::new(),
                    };
                    let bytes = match doc.get(&seg_obj, PRESERVED_SEG_BYTES) {
                        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                            ScalarValue::Bytes(b) => b.clone(),
                            _ => return None,
                        },
                        _ => return None,
                    };
                    Some((ord, sid, bytes))
                })
                .collect();
            // Sort by fractional order key, then segment id as a deterministic tiebreak.
            segs.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
            Some(segs.into_iter().flat_map(|(_, _, b)| b).collect())
        }
        // Legacy single-blob form: the whole part as one `Bytes` scalar.
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The per-atom segment ids of a preserved part, in source order, paired with each atom's verbatim bytes.
/// Empty for a part stored in the legacy single-blob form. Lets a codec or test address an individual atom by its stable id.
pub fn preserved_atoms(doc: &AutoCommit, c: &Containers, key: &str) -> Vec<(String, Vec<u8>)> {
    let part_obj = match doc.get(&c.preserved, key) {
        Ok(Some((Value::Object(ObjType::Map), o))) => o,
        _ => return Vec::new(),
    };
    let mut segs: Vec<(Vec<u8>, String, Vec<u8>)> = doc
        .keys(&part_obj)
        .filter_map(|sid| {
            let seg_obj = match doc.get(&part_obj, &sid) {
                Ok(Some((Value::Object(ObjType::Map), o))) => o,
                _ => return None,
            };
            let ord = match doc.get(&seg_obj, PRESERVED_SEG_ORD) {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::Bytes(b) => b.clone(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            let bytes = match doc.get(&seg_obj, PRESERVED_SEG_BYTES) {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::Bytes(b) => b.clone(),
                    _ => return None,
                },
                _ => return None,
            };
            Some((ord, sid, bytes))
        })
        .collect();
    segs.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
    segs.into_iter().map(|(_, sid, b)| (sid, b)).collect()
}

/// A stable, replica-independent id for a preserved atom: its source-order index plus a short content hash.
/// The index disambiguates repeated identical atoms; the content hash keeps the id meaningful across re-imports.
/// The same source bytes split into the same ids on every replica, so the same atom converges to one automerge object.
fn preserved_atom_id(idx: usize, atom: &[u8]) -> String {
    let h = blake3::hash(atom).to_hex();
    format!("{idx:06}-{}", &h.as_str()[..16])
}

/// Split a preserved part into independently-mergeable atoms whose concatenation is the original bytes verbatim.
/// For an XML part this is the sequence of top-level fragments — each top-level child element is its own atom,
/// with inter-element text riding on adjacent atoms — so replicas modifying different top-level elements touch
/// different atoms. A non-XML or unparseable part stays one whole-part atom. The split never changes the bytes.
pub fn split_preserved_atoms(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Only XML parts (`<?xml` prologue or a leading `<` tag) get split; everything
    // else (images, the mimetype literal, binary settings) is one atom.
    let looks_xml = {
        let head: &[u8] = &bytes[..bytes.len().min(64)];
        let trimmed = head
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(head, |p| &head[p..]);
        trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<")
    };
    if !looks_xml {
        return vec![bytes.to_vec()];
    }
    match split_xml_top_level(bytes) {
        Some(atoms) if atoms.len() > 1 => atoms,
        _ => vec![bytes.to_vec()],
    }
}

/// One parsed XML element span: its nesting `depth` (root = 0) and the byte offset `start` of its opening `<`.
struct ElemSpan {
    depth: usize,
    start: usize,
}

/// Parse the depth + start offset of every element in `bytes`. Returns `None` on any parse error or
/// imbalance (caller then keeps the part as a single atom — never corrupts it).
fn parse_elem_spans(bytes: &[u8]) -> Option<Vec<ElemSpan>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let s = std::str::from_utf8(bytes).ok()?;
    // quick-xml 0.36 defaults `expand_empty_elements` to false, so a self-closing
    // `<x/>` arrives as one `Event::Empty` rather than a Start+End pair.
    let mut reader = Reader::from_str(s);
    let mut depth: usize = 0;
    let mut spans: Vec<ElemSpan> = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(_)) => {
                spans.push(ElemSpan {
                    depth,
                    start: before,
                });
                depth += 1;
            }
            Ok(Event::Empty(_)) => {
                spans.push(ElemSpan {
                    depth,
                    start: before,
                });
            }
            Ok(Event::End(_)) => {
                depth = depth.checked_sub(1)?;
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    if depth != 0 {
        return None; // unbalanced
    }
    Some(spans)
}

/// Split XML bytes into per-element atoms, descending through single-child wrapper layers so the split lands
/// where the document actually *branches* (the first element with 2+ element children). E.g. for
/// `<doc><styles><a/><b/></styles></doc>` the atoms are the prologue+`<doc><styles>` open, `<a/>`, then
/// `<b/></styles></doc>`. Atoms always concatenate back to `bytes` byte-for-byte; `None` if no branch point exists or on parse error.
fn split_xml_top_level(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let spans = parse_elem_spans(bytes)?;
    if spans.is_empty() {
        return None;
    }
    // Find the shallowest depth that branches into 2+ sibling elements under a single shared parent — that's
    // where the independent unmodeled atoms live. We descend single-child wrapper layers transparently: a depth
    // is a valid split point only if its parent held exactly one element. Depth 0's "parent" is the implicit document root.
    let max_depth = spans.iter().map(|s| s.depth).max().unwrap_or(0);
    for d in 0..=max_depth {
        // Require a single shared parent: exactly one element at the parent depth.
        if d > 0 && spans.iter().filter(|s| s.depth == d - 1).count() != 1 {
            break; // the tree already branched above this depth — stop descending
        }
        // Elements at this depth, in source order.
        let mut at_d: Vec<&ElemSpan> = spans.iter().filter(|s| s.depth == d).collect();
        at_d.sort_by_key(|s| s.start);
        if at_d.len() < 2 {
            continue; // single wrapper (or none) at this depth — descend further
        }
        // Split at the boundaries between sibling elements: the leading atom is everything before the first sibling
        // (prologue + ancestor open tags); the final atom runs to EOF (last sibling + ancestor close tags + trailing text).
        let mut boundaries: Vec<usize> = Vec::new();
        for s in at_d.iter().skip(1) {
            boundaries.push(s.start);
        }
        if boundaries.is_empty() {
            continue;
        }
        let mut atoms: Vec<Vec<u8>> = Vec::with_capacity(boundaries.len() + 1);
        let mut prev = 0usize;
        for &b in &boundaries {
            atoms.push(bytes[prev..b].to_vec());
            prev = b;
        }
        atoms.push(bytes[prev..].to_vec());
        // Byte-fidelity guard: atoms MUST concatenate back to the source exactly.
        let total: usize = atoms.iter().map(std::vec::Vec::len).sum();
        if total != bytes.len() {
            return None;
        }
        return Some(atoms);
    }
    None
}

fn read_attrs(doc: &AutoCommit, block_obj: &ObjId) -> BTreeMap<String, AttrVal> {
    let attrs_obj = match doc.get(block_obj, BLOCK_ATTRS) {
        Ok(Some((Value::Object(ObjType::Map), a))) => a,
        _ => return BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    for key in doc.keys(&attrs_obj) {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(&attrs_obj, &key) {
            let val = match s.as_ref() {
                ScalarValue::Int(i) => AttrVal::Int(*i),
                ScalarValue::Uint(u) => AttrVal::Int(*u as i64),
                ScalarValue::Boolean(b) => AttrVal::Bool(*b),
                ScalarValue::Str(st) => AttrVal::Str(st.to_string()),
                ScalarValue::F64(f) => AttrVal::Float(*f),
                _ => continue,
            };
            out.insert(key, val);
        }
    }
    out
}

pub fn materialize(doc: &AutoCommit, c: &Containers) -> DocSnapshot {
    let blocks = blocks_in_order(doc, c)
        .into_iter()
        .map(|id| materialize_block(doc, &c.blocks, &id))
        .collect();
    let (columns, column_gap) = columns(doc, c);
    let mut snap = DocSnapshot {
        schema: SCHEMA_ID.to_string(),
        schema_version: SCHEMA_VERSION,
        blocks,
        blobs: blobs(doc, c),
        styles: all_styles(doc, c),
        properties: doc_props(doc, c),
        header: materialize_region(doc, c, Region::Header),
        footer: materialize_region(doc, c, Region::Footer),
        notes: materialize_notes(doc, c),
        comments: materialize_comments(doc, c),
        changes: materialize_changes(doc, c),
        bookmarks: bookmarks(doc, c),
        columns,
        column_gap: column_gap.map(|g| g as f32),
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
        cache: &mut BTreeMap<String, crate::blobstore::BlobMeta>,
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
            b.attrs
                .insert(ATTR_BLOB_BYTES.to_string(), AttrVal::Int(meta.len as i64));
            if meta.w > 0 && meta.h > 0 {
                b.attrs
                    .insert(ATTR_BLOB_W.to_string(), AttrVal::Int(i64::from(meta.w)));
                b.attrs
                    .insert(ATTR_BLOB_H.to_string(), AttrVal::Int(i64::from(meta.h)));
            }
            walk(doc, c, cache, &mut b.children);
        }
    }
    let mut cache: BTreeMap<String, crate::blobstore::BlobMeta> = BTreeMap::new();
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

/// Read a style sub-map's scalar properties into an [`AttrVal`] map.
fn read_style_props(doc: &AutoCommit, style_obj: &ObjId) -> BTreeMap<String, AttrVal> {
    let mut out = BTreeMap::new();
    for key in doc.keys(style_obj) {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(style_obj, &key) {
            let val = match s.as_ref() {
                ScalarValue::Int(i) => AttrVal::Int(*i),
                ScalarValue::Uint(u) => AttrVal::Int(*u as i64),
                ScalarValue::Boolean(b) => AttrVal::Bool(*b),
                ScalarValue::Str(st) => AttrVal::Str(st.to_string()),
                ScalarValue::F64(f) => AttrVal::Float(*f),
                _ => continue,
            };
            out.insert(key, val);
        }
    }
    out
}

/// Style ids defined in the document's styles container (e.g. `body`, `heading1`, or an imported ODF named-style name).
pub fn style_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    doc.keys(&c.styles)
        .filter(|id| {
            matches!(
                doc.get(&c.styles, id),
                Ok(Some((Value::Object(ObjType::Map), _)))
            )
        })
        .collect()
}

/// The scalar properties of a single style (`name`/`font`/`size`/… → value), or an empty map if the style id is unknown.
pub fn style_props(doc: &AutoCommit, c: &Containers, id: &str) -> BTreeMap<String, AttrVal> {
    match doc.get(&c.styles, id) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => read_style_props(doc, &obj),
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

fn materialize_block(doc: &AutoCommit, container: &ObjId, id: &str) -> BlockView {
    let obj = match doc.get(container, id) {
        Ok(Some((Value::Object(ObjType::Map), o))) => o,
        _ => {
            return BlockView {
                id: id.into(),
                kind: String::new(),
                text: String::new(),
                marks: vec![],
                attrs: BTreeMap::new(),
                children: vec![],
            };
        }
    };
    let kind = match doc.get(&obj, BLOCK_KIND) {
        Ok(Some((v, _))) => v.to_str().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let text = match doc.get(&obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
        _ => String::new(),
    };
    let marks = marks_on(doc, &obj);
    let attrs = read_attrs(doc, &obj);
    let children = match doc.get(&obj, BLOCK_CHILDREN) {
        Ok(Some((Value::Object(ObjType::Map), cid))) => ordered_keys(doc, &cid)
            .into_iter()
            .map(|cc| materialize_block(doc, &cid, &cc))
            .collect(),
        _ => vec![],
    };
    BlockView {
        id: id.into(),
        kind,
        text,
        marks,
        attrs,
        children,
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

impl DocMut<'_> {
    /// Resolve-or-create a region's block container. The body always exists; header/footer maps are created
    /// lazily on first write and cached back into `Containers` for later reads. Takes `&Containers` by
    /// value-copy of the relevant id so this doesn't need a mutable borrow of `self.c` (which is shared `&`).
    fn region_container(&mut self, region: Region) -> ObjId {
        match region {
            Region::Body => self.c.blocks.clone(),
            Region::Header => self.ensure_region(KEY_HEADER, self.c.header.clone()),
            Region::Footer => self.ensure_region(KEY_FOOTER, self.c.footer.clone()),
        }
    }

    fn ensure_region(&mut self, key: &str, cached: Option<ObjId>) -> ObjId {
        if let Some(obj) = cached {
            return obj;
        }
        // Re-check the live doc in case a prior op in this transaction created it.
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, key) {
            return obj;
        }
        self.doc
            .put_object(&self.c.root, key, ObjType::Map)
            .unwrap()
    }

    /// A fractional key ordered strictly after block `after` (or at the front when `after` is `None`) within
    /// `container`. Uses the in-memory order index so the lookup is O(log n), not an O(n log n) rescan.
    fn order_after_in(&mut self, container: &ObjId, after: Option<&str>) -> Vec<u8> {
        self.ensure_order_cache(container);
        let co = self
            .ids
            .order
            .by_container
            .get(container)
            .expect("order cache just built");
        let (lo, hi): (Option<Vec<u8>>, Option<Vec<u8>>) = match after {
            // Insert at the front: bounded above by the first block's key.
            None => (None, co.entries.iter().next().map(|k| k.key.clone())),
            Some(id) => match co.by_id.get(id) {
                // `after` exists: lo is its key, hi is the next block's key.
                Some(after_key) => {
                    let probe = OrderKey {
                        key: after_key.clone(),
                        id: id.to_string(),
                    };
                    let hi = co
                        .entries
                        .range((Bound::Excluded(probe), Bound::Unbounded))
                        .next()
                        .map(|k| k.key.clone());
                    (Some(after_key.clone()), hi)
                }
                // `after` unknown → append at the end (mirrors the old `unwrap_or(len)`).
                None => (co.entries.iter().next_back().map(|k| k.key.clone()), None),
            },
        };
        crate::order::key_between(lo.as_deref(), hi.as_deref())
    }

    fn order_after(&mut self, after: Option<&str>) -> Vec<u8> {
        let container = self.c.blocks.clone();
        self.order_after_in(&container, after)
    }

    /// Build the order index for `container` on first use (one O(n log n) scan), then keep it incrementally.
    fn ensure_order_cache(&mut self, container: &ObjId) {
        if self.ids.order.by_container.contains_key(container) {
            return;
        }
        let mut co = ContainerOrder::default();
        for id in ordered_keys(self.doc, container) {
            if let Some(key) = block_order(self.doc, container, &id) {
                co.by_id.insert(id.clone(), key.clone());
                co.entries.insert(OrderKey { key, id });
            }
        }
        self.ids.order.by_container.insert(container.clone(), co);
    }

    /// Record (or re-key) a block's order in the index. Called right after the block is written to the
    /// document, so the index stays in lock-step with it.
    fn record_order(&mut self, container: &ObjId, id: &str, key: &[u8]) {
        if let Some(co) = self.ids.order.by_container.get_mut(container) {
            if let Some(old) = co.by_id.insert(id.to_string(), key.to_vec()) {
                co.entries.remove(&OrderKey {
                    key: old,
                    id: id.to_string(),
                });
            }
            co.entries.insert(OrderKey {
                key: key.to_vec(),
                id: id.to_string(),
            });
        }
    }

    /// Drop a block from the order index (on delete). A no-op if the container's index hasn't been built
    /// yet — it will rebuild from the (already-updated) document on next use.
    fn forget_order(&mut self, container: &ObjId, id: &str) {
        if let Some(co) = self.ids.order.by_container.get_mut(container)
            && let Some(old) = co.by_id.remove(id)
        {
            co.entries.remove(&OrderKey {
                key: old,
                id: id.to_string(),
            });
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
    pub fn insert_block_in(&mut self, region: Region, after: Option<&str>, kind: &str) -> String {
        let container = self.region_container(region);
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
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
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
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
        let Some(container) = region_obj(self.doc, self.c, region) else {
            return;
        };
        let Some(obj) = block_obj(self.doc, &container, block_id) else {
            return;
        };
        let attrs = match self.doc.get(&obj, BLOCK_ATTRS) {
            Ok(Some((Value::Object(ObjType::Map), a))) => a,
            _ => return,
        };
        let _ = self.doc.put(&attrs, key, value);
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
            idx + 1,
            MARK_FIELD,
            FIELD_PAGE_NUMBER,
        );
    }

    /// Resolve-or-create the `notes` root container (created lazily on the first note insert).
    fn notes_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.notes.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_NOTES) {
            return obj;
        }
        self.doc
            .put_object(&self.c.root, KEY_NOTES, ObjType::Map)
            .unwrap()
    }

    /// Create a new note (footnote/endnote) with an empty block sequence and return its freshly-minted id
    /// (filled in via [`Self::insert_note_block`] + [`Self::splice_note_text`]); id shares the body's generator so it never collides.
    pub fn insert_note(&mut self, kind: NoteKind) -> String {
        let notes = self.notes_container();
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
        self.add_mark(block_id, idx, idx + 1, MARK_NOTE_REF, note_id);
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
    pub fn insert_note_block(
        &mut self,
        note_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let container = self.note_blocks_container(note_id)?;
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
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
        Some(id)
    }

    /// Resolve-or-create the Text object of a note's block.
    fn note_text_obj(&mut self, note_id: &str, block_id: &str) -> Option<ObjId> {
        let container = self.note_blocks_container(note_id)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    /// Splice text into a note's block.
    pub fn splice_note_text(
        &mut self,
        note_id: &str,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
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
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Resolve-or-create the `comments` root container (created lazily on the first comment insert).
    fn comments_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.comments.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_COMMENTS) {
            return obj;
        }
        self.doc
            .put_object(&self.c.root, KEY_COMMENTS, ObjType::Map)
            .unwrap()
    }

    /// Create a new comment with an `author`, a `date` string, and an empty block sequence; returns its
    /// freshly-minted id (shares the body's generator so it never collides). Content is filled via
    /// [`Self::insert_comment_block`] + [`Self::splice_comment_text`]; the body anchor via [`Self::add_comment_mark`].
    pub fn insert_comment(&mut self, author: &str, date: &str) -> String {
        let comments = self.comments_container();
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
    pub fn insert_comment_block(
        &mut self,
        comment_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let container = self.comment_blocks_container(comment_id)?;
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
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
        Some(id)
    }

    /// Resolve-or-create the Text object of a comment's block.
    fn comment_text_obj(&mut self, comment_id: &str, block_id: &str) -> Option<ObjId> {
        let container = self.comment_blocks_container(comment_id)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    /// Splice text into a comment's block.
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

    /// Resolve-or-create the `changes` root container (created lazily on the first tracked change).
    fn changes_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.changes.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_CHANGES) {
            return obj;
        }
        self.doc
            .put_object(&self.c.root, KEY_CHANGES, ObjType::Map)
            .unwrap()
    }

    /// Create a tracked-change record of `kind` with an `author` + `date`; returns its freshly-minted id (shares
    /// the body's generator). The body anchor is placed separately via [`Self::add_change_mark`] — callers usually go through [`Self::track_insert`]/[`Self::track_delete`].
    pub fn insert_change(
        &mut self,
        kind: crate::snapshot::ChangeKind,
        author: &str,
        date: &str,
    ) -> String {
        let changes = self.changes_container();
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
                char_idx + n,
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
                    self.splice_text(&b, start, (end - start) as isize, "");
                }
            }
        }
        self.remove_change_record(change_id);
    }

    /// Reject a tracked change, reverting it (inverse of [`Self::accept_change`]): an **insertion** removes the
    /// inserted text (and its `ins` mark); a **deletion** keeps the struck text and drops its `del` mark. The
    /// change record is removed from the `changes` container either way. A no-op for an unknown id.
    pub fn reject_change(&mut self, change_id: &str) {
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
                    self.splice_text(&b, start, (end - start) as isize, "");
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
        if let Some(changes) = changes_obj(self.doc, self.c) {
            let _ = self.doc.delete(&changes, change_id);
        }
    }

    // ---- bookmarks / cross-references -------------------------------------

    /// Anchor a **bookmark** (named anchor) over `[start, end)` of `block_id`: cover it with a `bookmark` mark
    /// valued with `name`. A zero-width (point) bookmark is widened to one character when the block has text
    /// after the anchor (a mark needs a non-empty range to live on); an end-of-block point sits over the last character.
    pub fn add_bookmark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        let (mut s, mut e) = (start, end);
        if e <= s {
            // Widen a point bookmark to one character so the mark has somewhere to live.
            let blen = crate::model::block_text(self.doc, self.c, block_id)
                .map_or(0, |t| t.chars().count());
            if s < blen {
                e = s + 1;
            } else if blen > 0 {
                // End-of-block point: sit over the last character.
                s = blen - 1;
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
        let Some(container) = region_obj(self.doc, self.c, region) else {
            return;
        };
        for id in ordered_keys(self.doc, &container) {
            let _ = self.doc.delete(&container, &id);
        }
        // Drop the (now-empty) container's cached order; it rebuilds lazily.
        self.ids.order.by_container.remove(&container);
    }

    /// Resolve-or-create the Text object of a region block.
    fn region_text_obj(&mut self, region: Region, block_id: &str) -> Option<ObjId> {
        let container = region_obj(self.doc, self.c, region)?;
        let obj = block_obj(self.doc, &container, block_id)?;
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    pub fn remove_block(&mut self, id: &str) {
        let _ = self.doc.delete(&self.c.blocks, id);
        let container = self.c.blocks.clone();
        self.forget_order(&container, id);
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        // Compute the target key with the block still at its old position (mirrors the pre-index behaviour).
        let key = self.order_after(after);
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
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    pub fn splice_text(&mut self, block_id: &str, idx: usize, del: isize, ins: &str) {
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
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Set a scalar attribute (e.g. heading `level`, list `ordered`) in the block's `attrs` map.
    pub fn set_attr(&mut self, block_id: &str, key: &str, value: impl Into<ScalarValue>) {
        let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) else {
            return;
        };
        let attrs = match self.doc.get(&obj, BLOCK_ATTRS) {
            Ok(Some((Value::Object(ObjType::Map), a))) => a,
            _ => return,
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
    /// [`DOC_PROP_KEYS`]). Each is its own scalar register in `meta.props`, so replicas editing different properties field-level merge.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        let props = self.props_map();
        let _ = self.doc.put(&props, name, value);
    }

    /// Upsert a named style into the styles container. Each property is written as its own scalar register
    /// *into the existing sub-map*, so replicas editing different properties of the same style field-level merge.
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

    /// Change a block's kind (e.g. `paragraph` ↔ `heading` ↔ `list_item`).
    pub fn set_kind(&mut self, block_id: &str, kind: &str) {
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) {
            let _ = self.doc.put(&obj, BLOCK_KIND, kind);
        }
    }

    /// Remove a mark named `name` over `[start, end)` of the block's text.
    pub fn remove_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.unmark(&tid, name, start, end, ExpandMark::After);
        }
    }

    /// Stash a verbatim blob in the preservation layer (e.g. an unmodeled ODF part), keyed by `key`; re-emitted
    /// unchanged on export. This is the *segmented* (Tier-4, CRDT-merge-friendly) entry point: `bytes` is split
    /// into independently-mergeable atoms (top-level XML child elements, or a single whole-part atom) stored as
    /// `Map<segment_id, {ord, bytes}>`. The same source splits into the same stable ids on every replica, so two
    /// replicas modifying different unmodeled atoms of the same part now merge instead of clobbering; reconstruction via [`preserved_part`] reproduces `bytes` byte-for-byte.
    pub fn set_preserved(&mut self, key: &str, bytes: &[u8]) {
        self.set_preserved_atoms(key, split_preserved_atoms(bytes));
    }

    /// Stash a preserved part already split into ordered atoms (verbatim byte slices that concatenate back to
    /// the whole part). Each atom becomes an independent automerge object keyed by a stable id, so distinct atoms merge across replicas. Replaces any existing part under `key`.
    pub fn set_preserved_atoms(&mut self, key: &str, atoms: Vec<Vec<u8>>) {
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
            let ord = (idx as u32).to_be_bytes().to_vec();
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
        let part = match self.doc.get(&self.c.preserved, key) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => return,
        };
        if let Ok(Some((Value::Object(ObjType::Map), seg))) = self.doc.get(&part, segment_id) {
            let _ = self.doc.put(
                &seg,
                PRESERVED_SEG_BYTES,
                ScalarValue::Bytes(bytes.to_vec()),
            );
        }
    }

    /// Store `bytes` in the content-addressed blob store and write a descriptor under their blake3 hex hash
    /// (the key an `image` block's `blob` attr references). Creates the blobs map lazily for older documents.
    pub fn put_blob(&mut self, bytes: &[u8]) -> String {
        let hash = crate::blobstore::put(bytes);
        let blobs = match blobs_obj(self.doc, self.c) {
            Some(b) => b,
            None => self
                .doc
                .put_object(&self.c.root, KEY_BLOBS, ObjType::Map)
                .unwrap(),
        };
        // Idempotent by construction: the key is the content hash, so identical bytes address the same descriptor
        // and re-putting it would be pure tombstone churn for no change in meaning.
        if matches!(
            self.doc.get(&blobs, hash.as_str()),
            Ok(Some((Value::Object(ObjType::Map), _)))
        ) {
            return hash;
        }
        let meta = crate::blobstore::sniff(bytes);
        if let Ok(d) = self.doc.put_object(&blobs, hash.as_str(), ObjType::Map) {
            let _ = self
                .doc
                .put(&d, BLOB_MIME, ScalarValue::Str(meta.mime.as_str().into()));
            let _ = self
                .doc
                .put(&d, BLOB_LEN, ScalarValue::Int(meta.len as i64));
            let _ = self
                .doc
                .put(&d, BLOB_W, ScalarValue::Int(i64::from(meta.w)));
            let _ = self
                .doc
                .put(&d, BLOB_H, ScalarValue::Int(i64::from(meta.h)));
        }
        hash
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
    fn styles_container_crud_and_snapshot() {
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
        got.sort();
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

    #[test]
    fn doc_props_crud_and_snapshot() {
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

    #[test]
    fn open_is_backward_compatible_without_props_map() {
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

    #[test]
    fn set_style_is_field_level_upsert() {
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

#[cfg(test)]
mod genesis_detection_tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ObjType, ROOT};

    /// The two helpers a caller uses to tell a schema change from a person's edit have no consumer in this
    /// crate — the version view that filters on them lives elsewhere — so without these tests they can drift unnoticed.
    #[test]
    fn the_genesis_change_is_told_apart_from_an_authored_one() {
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
        let authored: Vec<_> = changes
            .iter()
            .filter(|ch| !is_schema_genesis_actor(ch.actor_id()))
            .collect();

        assert!(
            !genesis.is_empty(),
            "the schema has to be written by the genesis actor"
        );
        assert!(
            !authored.is_empty(),
            "the edit has to be attributed to this replica"
        );
        assert_eq!(
            genesis[0].actor_id().to_hex_string(),
            schema_genesis_actor_hex(),
            "the hex form must match what a recorded actor_hex is compared against"
        );
    }

    /// A document created before the genesis existed, merged with one created after it, is the case the genesis
    /// was introduced to prevent: two candidates per container key, one of them unreachable for good.
    #[test]
    fn a_second_minting_of_the_schema_is_reported_as_shadowed() {
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
        merged.merge(&mut own).unwrap();
        let shadowed = shadowed_containers(&merged, &ROOT);
        assert!(
            shadowed.contains(&KEY_BLOCKS.to_string()),
            "two candidates for {KEY_BLOCKS:?} must be reported, got {shadowed:?}"
        );
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;

    use automerge::{AutoCommit, ROOT};

    #[test]
    fn header_footer_are_backward_compatible_and_shared() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        // The regions come with the schema rather than being minted on first
        // write, so two people who each add a page header cannot end up with two
        // `root.header` maps and lose one — see `GENESIS_ACTOR`.
        assert!(
            c.header.is_some() && c.footer.is_some(),
            "regions come with the schema"
        );
        assert!(region_blocks_in_order(&doc, &c, Region::Header).is_empty());
        assert!(materialize_region(&doc, &c, Region::Footer).is_empty());
        let bytes = doc.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(region_blocks_in_order(&reloaded, &c2, Region::Header).is_empty());
        assert!(materialize_region(&reloaded, &c2, Region::Footer).is_empty());

        // A document authored before the regions existed has no such keys at all; it must still open and read as empty.
        let mut old = AutoCommit::new();
        let _ = init(&mut old, &ROOT);
        old.delete(&ROOT, KEY_HEADER).unwrap();
        old.delete(&ROOT, KEY_FOOTER).unwrap();
        old.commit();
        let mut reopened = AutoCommit::load(&old.save()).unwrap();
        let oc = open(&reopened, &ROOT).unwrap();
        assert!(
            oc.header.is_none() && oc.footer.is_none(),
            "an older document has no regions"
        );
        assert!(region_blocks_in_order(&reopened, &oc, Region::Header).is_empty());
        let mut ids = IdGen::new(reopened.get_actor().to_hex_string());
        {
            let mut dm = DocMut {
                doc: &mut reopened,
                c: &oc,
                ids: &mut ids,
            };
            let b = dm.insert_block_in(Region::Header, None, "paragraph");
            dm.splice_text_in(Region::Header, &b, 0, 0, "migrated");
        }
        let migrated = open(&reopened, &ROOT).unwrap();
        assert_eq!(
            materialize_region(&reopened, &migrated, Region::Header)[0].text,
            "migrated",
            "an older document still takes a header"
        );
    }

    /// The reason the regions are not lazily created: two replicas each adding a
    /// page header must end up with both, not with whichever one won a
    /// comparison of their actor ids.
    #[test]
    fn concurrent_headers_on_two_replicas_both_survive() {
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
            a.merge(&mut b).unwrap();
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

    #[test]
    fn header_footer_blocks_and_page_number_field() {
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

    #[test]
    fn notes_container_is_empty_on_fresh_doc_and_backward_compatible() {
        use automerge::transaction::Transactable;
        // A fresh doc has an (empty) notes container — created eagerly so concurrent
        // footnote inserts converge on one shared object.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        assert!(c.notes.is_some(), "notes container present on a fresh doc");
        assert!(note_ids(&doc, &c).is_empty(), "but empty");
        assert!(materialize_notes(&doc, &c).is_empty());

        // Backward-compat: a document authored before the notes map existed (no root.notes) must still open and read as having no notes.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc; // simulate the old schema by deleting the eagerly-created map
        old.delete(&ROOT, KEY_NOTES).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(
            c2.notes.is_none(),
            "old-schema doc opens with no notes container"
        );
        assert!(note_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn footnote_marker_and_note_content() {
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

    #[test]
    fn seeded_id_gen_does_not_remint_note_ids() {
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

    #[test]
    fn comments_container_is_empty_on_fresh_doc_and_backward_compatible() {
        use automerge::transaction::Transactable;
        // A fresh doc has an (empty) comments container — eager, so concurrent comment adds converge on one shared object.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        assert!(
            c.comments.is_some(),
            "comments container present on a fresh doc"
        );
        assert!(comment_ids(&doc, &c).is_empty(), "but empty");
        assert!(materialize_comments(&doc, &c).is_empty());

        // Backward-compat: a document authored before the comments map existed must still open and read as having no comments.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc;
        old.delete(&ROOT, KEY_COMMENTS).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(
            c2.comments.is_none(),
            "old-schema doc opens with no comments container"
        );
        assert!(comment_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn comment_anchor_and_content() {
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

    #[test]
    fn seeded_id_gen_does_not_remint_comment_ids() {
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

    #[test]
    fn changes_container_is_empty_on_fresh_doc_and_backward_compatible() {
        use automerge::transaction::Transactable;
        // A fresh doc has an (empty) changes container — eager, for the same convergence reason as comments/notes.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        assert!(
            c.changes.is_some(),
            "changes container present on a fresh doc"
        );
        assert!(change_ids(&doc, &c).is_empty(), "but empty");
        assert!(materialize_changes(&doc, &c).is_empty());

        // Backward-compat: a document authored before the changes map existed must still open and read as having no changes.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc;
        old.delete(&ROOT, KEY_CHANGES).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(
            c2.changes.is_none(),
            "old-schema doc opens with no changes container"
        );
        assert!(change_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn tracked_insert_and_delete_anchor_and_metadata() {
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

    #[test]
    fn accept_insertion_clears_mark_keeps_text() {
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

    #[test]
    fn accept_deletion_removes_text() {
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

    #[test]
    fn reject_inverts_accept() {
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

    #[test]
    fn seeded_id_gen_does_not_remint_change_ids() {
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

    #[test]
    fn bookmark_anchor_cross_ref_and_snapshot() {
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

    #[test]
    fn point_bookmark_is_widened_and_heading_ref_id_targets_heading() {
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

    #[test]
    fn bookmarks_converge_across_replicas() {
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

    #[test]
    fn clear_region_empties_it() {
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

    #[test]
    fn split_concatenates_back_verbatim() {
        let atoms = split_preserved_atoms(TWO_STYLE_PART);
        // Two depth-1 siblings → two atoms: [prologue + root-open + style A], [style B + root-close]
        // (the first sibling rides with the leading material; each subsequent sibling opens a new atom).
        assert_eq!(atoms.len(), 2, "two style atoms: {atoms:?}");
        let joined: Vec<u8> = atoms.iter().flatten().copied().collect();
        assert_eq!(
            joined, TWO_STYLE_PART,
            "atoms must concatenate to the source byte-for-byte"
        );
        let has_a = atoms.iter().any(|a| a.windows(5).any(|w| w == b"alpha"));
        let has_b = atoms.iter().any(|a| a.windows(5).any(|w| w == b"bravo"));
        assert!(has_a && has_b, "both style atoms present in the split");
        let same_atom = atoms
            .iter()
            .any(|a| a.windows(5).any(|w| w == b"alpha") && a.windows(5).any(|w| w == b"bravo"));
        assert!(!same_atom, "the two styles must be in SEPARATE atoms");
    }

    #[test]
    fn non_xml_part_stays_single_atom() {
        let png = &[0x89u8, b'P', b'N', b'G', 0, 1, 2, 3][..];
        let atoms = split_preserved_atoms(png);
        assert_eq!(atoms, vec![png.to_vec()], "binary part is one opaque atom");
    }

    #[test]
    fn segmented_part_round_trips_verbatim() {
        let (mut doc, c, mut ids) = new_doc();
        {
            let mut dm = DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.set_preserved("odt/part/styles.xml", TWO_STYLE_PART);
        }
        // Stored as a segmented Map, yet reconstructs byte-for-byte.
        let got = preserved_part(&doc, &c, "odt/part/styles.xml").unwrap();
        assert_eq!(got, TWO_STYLE_PART, "segmented part re-emits verbatim");
        let parts = preserved_parts(&doc, &c);
        let p = parts
            .iter()
            .find(|(k, _)| k == "odt/part/styles.xml")
            .unwrap();
        assert_eq!(p.1, TWO_STYLE_PART);
        assert!(preserved_atoms(&doc, &c, "odt/part/styles.xml").len() >= 2);
    }

    #[test]
    fn legacy_blob_form_still_reconstructs() {
        // Simulate a document written under the old single-`Bytes`-scalar schema.
        let (mut doc, c, _ids) = new_doc();
        doc.put(
            &c.preserved,
            "odt/part/settings.xml",
            ScalarValue::Bytes(TWO_STYLE_PART.to_vec()),
        )
        .unwrap();
        let got = preserved_part(&doc, &c, "odt/part/settings.xml").unwrap();
        assert_eq!(got, TWO_STYLE_PART, "legacy blob still re-emits verbatim");
        // Legacy form exposes no addressable atoms (it's one opaque blob).
        assert!(preserved_atoms(&doc, &c, "odt/part/settings.xml").is_empty());
    }

    /// THE core win: two replicas import the same part, each rewrites a DIFFERENT
    /// unmodeled atom of it, then merge — and BOTH edits survive. Under the old
    /// whole-part `Bytes` scalar this was last-writer-wins (one edit clobbered the
    /// other); per-atom storage makes the two edits independent automerge objects.
    #[test]
    fn different_atom_edits_merge_not_clobber() {
        let (mut doc, c, mut ids) = new_doc();
        {
            let mut dm = DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            };
            dm.set_preserved("odt/part/styles.xml", TWO_STYLE_PART);
        }
        doc.commit();

        // Fork into replica B (distinct actor); A and B share the same per-atom object ids from the common ancestor.
        let mut doc_b = AutoCommit::load(&doc.save()).unwrap();
        doc_b.set_actor(automerge::ActorId::random());
        let c_b = open(&doc_b, &ROOT).unwrap();

        let atoms_a = preserved_atoms(&doc, &c, "odt/part/styles.xml");
        let id_alpha = atoms_a
            .iter()
            .find(|(_, b)| b.windows(5).any(|w| w == b"alpha"))
            .map(|(id, _)| id.clone())
            .unwrap();
        let id_bravo = atoms_a
            .iter()
            .find(|(_, b)| b.windows(5).any(|w| w == b"bravo"))
            .map(|(id, _)| id.clone())
            .unwrap();
        let atoms_b = preserved_atoms(&doc_b, &c_b, "odt/part/styles.xml");
        assert!(
            atoms_b.iter().any(|(id, _)| id == &id_alpha)
                && atoms_b.iter().any(|(id, _)| id == &id_bravo),
            "fork shares the same stable atom ids: A={atoms_a:?} B={atoms_b:?}"
        );

        // Replica A rewrites ONLY the alpha atom; replica B rewrites ONLY bravo. Read-modify-write the atom (keeping
        // its framing), mirroring how a codec would re-model one region in place.
        let edit_atom = |bytes: &[u8], from: &str, to: &str| -> Vec<u8> {
            String::from_utf8_lossy(bytes)
                .replace(from, to)
                .into_bytes()
        };
        let cur_alpha = atoms_a
            .iter()
            .find(|(id, _)| id == &id_alpha)
            .unwrap()
            .1
            .clone();
        let new_alpha = edit_atom(&cur_alpha, "alpha", "ALPHA-EDITED");
        let cur_bravo = atoms_a
            .iter()
            .find(|(id, _)| id == &id_bravo)
            .unwrap()
            .1
            .clone();
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

        // Merge both directions via the low-level Automerge (mirroring `Document::merge`, which sidesteps an
        // automerge-0.8 AutoCommit::merge patch-log panic). Reload each side from the merged bytes.
        let merge_into = |dst: &mut AutoCommit, src: &mut AutoCommit| -> AutoCommit {
            let mut a = automerge::Automerge::load(&dst.save()).unwrap();
            let mut b = automerge::Automerge::load(&src.save()).unwrap();
            let _ = a.merge(&mut b);
            AutoCommit::load(&a.save()).unwrap()
        };
        let merged_a = merge_into(&mut doc, &mut doc_b);
        let merged_b = merge_into(&mut doc_b, &mut doc);
        doc = merged_a;
        doc_b = merged_b;
        let c = open(&doc, &ROOT).unwrap();
        let c_b = open(&doc_b, &ROOT).unwrap();

        // BOTH edits survive on BOTH replicas — this is what clobbered before.
        for (d, cc, who) in [(&doc, &c, "A"), (&doc_b, &c_b, "B")] {
            let merged = preserved_part(d, cc, "odt/part/styles.xml").unwrap();
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
            preserved_part(&doc, &c, "odt/part/styles.xml"),
            preserved_part(&doc_b, &c_b, "odt/part/styles.xml"),
            "replicas converge on identical reconstructed bytes"
        );
    }
}
