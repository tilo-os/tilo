//! Borrowed-view model operations over a `&mut AutoCommit`, so a document can
//! live in any host's automerge doc (e.g. `ui::State.doc`) — not only inside an
//! owned [`crate::Document`]. The owned `Document` is a thin wrapper that
//! delegates here.

use crate::document::{DocError, MarkSpan};
use crate::schema::*;
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// Resolved object ids for the canonical container maps under a document root.
#[derive(Clone, Debug)]
pub struct Containers {
    /// The document root these containers live under. Needed so the blobs map can
    /// be created lazily for documents opened before the blobs map existed (the
    /// root is not always `automerge::ROOT` — a host may nest the model).
    pub root: ObjId,
    pub blocks: ObjId,
    pub styles: ObjId,
    pub preserved: ObjId,
    pub meta: ObjId,
    /// Content-addressed binary store (blake3 hex → `Bytes`). `None` for documents
    /// saved under an older schema that predates the blobs map; created lazily on
    /// the first [`DocMut::put_blob`].
    pub blobs: Option<ObjId>,
    /// Page-header block container (same shape as `blocks`). `None` for documents
    /// authored before the region existed; created lazily on the first header
    /// write so older documents migrate transparently.
    pub header: Option<ObjId>,
    /// Page-footer block container (same shape as `blocks`). `None` / lazily
    /// created, mirroring `header`.
    pub footer: Option<ObjId>,
    /// Footnote/endnote container (`Map<note_id, note>`). `None` for documents
    /// authored before notes existed; created lazily on the first note insert so
    /// older documents migrate transparently. Mirrors `header`/`footer`.
    pub notes: Option<ObjId>,
    /// Comment/annotation container (`Map<comment_id, comment>`). Eager-initialised
    /// like `notes` (so concurrent comment adds converge on one shared object);
    /// `None` only for documents authored before the comments map existed (created
    /// lazily on the first comment insert so they migrate transparently).
    pub comments: Option<ObjId>,
    /// Tracked-changes container (`Map<change_id, change>`). Eager-initialised like
    /// `comments` (so concurrent change tracking converges on one shared object);
    /// `None` only for documents authored before the changes map existed (created
    /// lazily on the first tracked change so they migrate transparently).
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

/// Which kind of note: a footnote (rendered at the bottom of its page) or an
/// endnote (rendered at the end of the document). Stored as the note's `kind`
/// scalar and round-tripped through the codecs.
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

/// Per-replica, collision-free block id generator: `{actor_hex}.{counter}`.
pub struct IdGen {
    actor_hex: String,
    next: u64,
}

impl IdGen {
    pub fn new(actor_hex: String) -> Self {
        Self { actor_hex, next: 0 }
    }
    /// Start the counter at `next` — used to seed past ids that already exist in
    /// a loaded/opened document (otherwise a fresh generator re-mints existing
    /// ids and overwrites those blocks).
    pub fn with_counter(actor_hex: String, next: u64) -> Self {
        Self { actor_hex, next }
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
        if let Some(rest) = id.strip_prefix(&prefix) {
            if let Ok(n) = rest.parse::<u64>() {
                next = next.max(n + 1);
            }
        }
    };
    for id in blocks_in_order(doc, c) {
        bump(&id);
    }
    // Header/footer blocks, note ids, and note-body blocks all draw from the same
    // per-replica generator, so a generator seeded over a reloaded document must
    // scan them too — otherwise it could re-mint (and overwrite) an existing note
    // or region block.
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
    // Comment ids + comment-body block ids draw from the same generator, so a seeded
    // generator must skip them too (otherwise it could re-mint an existing comment).
    for cid in comment_ids(doc, c) {
        bump(&cid);
        for bid in comment_blocks_in_order(doc, c, &cid) {
            bump(&bid);
        }
    }
    // Tracked-change ids draw from the same generator, so a seeded generator must skip
    // them too (otherwise it could re-mint an existing change).
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

/// Write schema scalars + create the container maps under `root`.
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> Containers {
    doc.put(root, KEY_SCHEMA, SCHEMA_ID).unwrap();
    doc.put(root, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
    let blocks = doc.put_object(root, KEY_BLOCKS, ObjType::Map).unwrap();
    let styles = doc.put_object(root, KEY_STYLES, ObjType::Map).unwrap();
    let preserved = doc.put_object(root, KEY_PRESERVED, ObjType::Map).unwrap();
    let meta = doc.put_object(root, KEY_META, ObjType::Map).unwrap();
    let blobs = doc.put_object(root, KEY_BLOBS, ObjType::Map).unwrap();
    // The notes container is created eagerly (like blocks/styles) so two replicas
    // that concurrently add a footnote share ONE container object and converge —
    // a lazily-created map would be a per-replica object, and automerge's
    // concurrent-object-creation conflict would drop one replica's notes. Documents
    // authored before the notes map existed still open (it reads as `None` and is
    // created lazily on first write — see `notes_container`).
    let notes = doc.put_object(root, KEY_NOTES, ObjType::Map).unwrap();
    // The comments container is created eagerly for the same convergence reason as
    // `notes`: two replicas concurrently adding a comment must share ONE container
    // object. Documents authored before it existed still open (`opt_map_at` reads it
    // as `None`, and `comments_container` creates it lazily on first write).
    let comments = doc.put_object(root, KEY_COMMENTS, ObjType::Map).unwrap();
    // The changes container is created eagerly for the same convergence reason as
    // `comments`: two replicas concurrently tracking a change must share ONE container
    // object. Documents authored before it existed still open (`opt_map_at` reads it as
    // `None`, and `changes_container` creates it lazily on first write).
    let changes = doc.put_object(root, KEY_CHANGES, ObjType::Map).unwrap();
    // Header/footer regions are created lazily on first write (a fresh document has
    // neither), so they stay `None` here.
    Containers { root: root.clone(), blocks, styles, preserved, meta, blobs: Some(blobs), header: None, footer: None, notes: Some(notes), comments: Some(comments), changes: Some(changes) }
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

/// Resolve a region's block container object, preferring the cached id and falling
/// back to a fresh root lookup (so a lazily-created header/footer map is still
/// found via reads). The body container always exists; header/footer may be
/// `None` until first written.
fn region_obj(doc: &AutoCommit, c: &Containers, region: Region) -> Option<ObjId> {
    match region {
        Region::Body => Some(c.blocks.clone()),
        Region::Header => c.header.clone().or_else(|| opt_map_at(doc, &c.root, KEY_HEADER)),
        Region::Footer => c.footer.clone().or_else(|| opt_map_at(doc, &c.root, KEY_FOOTER)),
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
        Some(obj) => ordered_keys(doc, &obj).into_iter().map(|id| materialize_block(doc, &obj, &id)).collect(),
        None => Vec::new(),
    }
}

/// Resolve the notes container object, preferring the cached id and falling back to
/// a fresh root lookup (so a lazily-created notes map is still found via reads).
fn notes_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.notes.clone().or_else(|| opt_map_at(doc, &c.root, KEY_NOTES))
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

/// The note ids present in the document, in a stable (sorted) order. The order is
/// canonical so two replicas converge byte-identically; the citation order in the
/// body text is recovered separately from the `note-ref` mark positions.
pub fn note_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match notes_obj(doc, c) {
        Some(notes) => {
            let mut ids: Vec<String> = doc
                .keys(&notes)
                .filter(|id| matches!(doc.get(&notes, id), Ok(Some((Value::Object(ObjType::Map), _)))))
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

/// Materialize a note's block sequence (empty if the note has no blocks / is
/// unknown).
pub fn materialize_note_blocks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<BlockView> {
    match note_blocks_obj(doc, c, id) {
        Some(obj) => ordered_keys(doc, &obj).into_iter().map(|bid| materialize_block(doc, &obj, &bid)).collect(),
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
            kind: note_kind(doc, c, &id).unwrap_or(NoteKind::Footnote).as_str().to_string(),
            blocks: materialize_note_blocks(doc, c, &id),
            id,
        })
        .collect()
}

// ---- comments / annotations ------------------------------------------------

/// Resolve the comments container object, preferring the cached id and falling back
/// to a fresh root lookup (so a lazily-created comments map is still found via reads).
fn comments_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.comments.clone().or_else(|| opt_map_at(doc, &c.root, KEY_COMMENTS))
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

/// The comment ids present in the document, in a stable (sorted) order. Canonical so
/// two replicas that merged in different orders materialize byte-identically; the
/// anchored ranges are recovered separately from the body `comment` marks.
pub fn comment_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match comments_obj(doc, c) {
        Some(comments) => {
            let mut ids: Vec<String> = doc
                .keys(&comments)
                .filter(|id| matches!(doc.get(&comments, id), Ok(Some((Value::Object(ObjType::Map), _)))))
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
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// A comment's date / timestamp string (empty if unset / unknown id).
pub fn comment_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let comment = comment_obj(doc, c, id)?;
    match doc.get(&comment, COMMENT_DATE) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
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

/// Materialize a comment's block sequence (empty if the comment has no blocks / is
/// unknown).
pub fn materialize_comment_blocks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<BlockView> {
    match comment_blocks_obj(doc, c, id) {
        Some(obj) => ordered_keys(doc, &obj).into_iter().map(|bid| materialize_block(doc, &obj, &bid)).collect(),
        None => Vec::new(),
    }
}

/// The anchored body range `(block_id, start, end)` of a comment, recovered from the
/// `comment` mark covering text whose value is the comment id. `None` if no such mark
/// is present (e.g. the commented text was deleted). Scans only the body container.
pub fn comment_range(doc: &AutoCommit, c: &Containers, id: &str) -> Option<(String, usize, usize)> {
    for bid in blocks_in_order(doc, c) {
        for m in marks(doc, c, &bid) {
            if m.name == MARK_COMMENT && m.value == crate::document::MarkValue::Str(id.to_string()) {
                return Some((bid, m.start, m.end));
            }
        }
    }
    None
}

/// Materialize every comment into a [`crate::snapshot::CommentView`] (sorted by id),
/// recovering each comment's anchored body range from the `comment` marks.
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

// ---- bookmarks / cross-references ------------------------------------------

/// Every bookmark in the body, recovered from the `bookmark` marks (each a named
/// anchor over a range). Sorted by `(name, block, start)` so two replicas that merged
/// in different orders materialize byte-identically. A name may appear more than once
/// only if duplicated by the source document; each occurrence is its own entry.
pub fn bookmarks(doc: &AutoCommit, c: &Containers) -> Vec<crate::snapshot::BookmarkView> {
    let mut out: Vec<crate::snapshot::BookmarkView> = Vec::new();
    for bid in blocks_in_order(doc, c) {
        for m in marks(doc, c, &bid) {
            if m.name == MARK_BOOKMARK {
                if let crate::document::MarkValue::Str(name) = &m.value {
                    out.push(crate::snapshot::BookmarkView { name: name.clone(), range: (bid.clone(), m.start, m.end) });
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.name, &a.range.0, a.range.1, a.range.2).cmp(&(&b.name, &b.range.0, b.range.1, b.range.2)));
    out
}

/// The auto/derived reference id of a heading block (so a cross-reference can target a
/// heading that carries no explicit bookmark): the [`HEADING_REF_PREFIX`] followed by
/// the block id. Stable across save/load (block ids are stable) and distinct from any
/// user bookmark name.
pub fn heading_ref_id(block_id: &str) -> String {
    format!("{HEADING_REF_PREFIX}{block_id}")
}

// ---- tracked changes / revision marks --------------------------------------

/// Resolve the changes container object, preferring the cached id and falling back to a
/// fresh root lookup (so a lazily-created changes map is still found via reads).
fn changes_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    c.changes.clone().or_else(|| opt_map_at(doc, &c.root, KEY_CHANGES))
}

/// Resolve a single change's sub-map object by id.
fn change_obj(doc: &AutoCommit, c: &Containers, id: &str) -> Option<ObjId> {
    let changes = changes_obj(doc, c)?;
    match doc.get(&changes, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

/// The change ids present in the document, in a stable (sorted) order. Canonical so two
/// replicas that merged in different orders materialize byte-identically; the anchored
/// ranges are recovered separately from the body `ins`/`del` marks.
pub fn change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    match changes_obj(doc, c) {
        Some(changes) => {
            let mut ids: Vec<String> = doc
                .keys(&changes)
                .filter(|id| matches!(doc.get(&changes, id), Ok(Some((Value::Object(ObjType::Map), _)))))
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
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// A change's date / timestamp string (empty if unset / unknown id).
pub fn change_date(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let change = change_obj(doc, c, id)?;
    match doc.get(&change, CHANGE_DATE) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// A change's kind (`Ins`/`Del`), or `None` if the id is unknown.
pub fn change_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<crate::snapshot::ChangeKind> {
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

/// The anchored body range `(block_id, start, end)` of a change, recovered from the
/// `ins`/`del` mark (matching the change's kind) covering text whose value is the change
/// id. `None` if no such mark is present (e.g. the change was accepted/rejected). Scans
/// only the body container.
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

/// Materialize every tracked change into a [`crate::snapshot::ChangeView`] (sorted by
/// id), recovering each change's anchored body range from the `ins`/`del` marks.
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

/// Fetch a content-addressed blob by its blake3 hex hash.
pub fn blob(doc: &AutoCommit, c: &Containers, hash: &str) -> Option<Vec<u8>> {
    let blobs = blobs_obj(doc, c)?;
    match doc.get(&blobs, hash) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// All content-addressed blobs (`blake3_hex` → bytes).
pub fn blobs(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if let Some(blobs) = blobs_obj(doc, c) {
        for k in doc.keys(&blobs) {
            if let Ok(Some((Value::Scalar(s), _))) = doc.get(&blobs, &k) {
                if let ScalarValue::Bytes(b) = s.as_ref() {
                    out.insert(k, b.clone());
                }
            }
        }
    }
    out
}

// ---- reads -----------------------------------------------------------------

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

fn ordered_keys(doc: &AutoCommit, container: &ObjId) -> Vec<String> {
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

/// Read a document-level meta string (e.g. the serialized stylesheet).
pub fn meta_str(doc: &AutoCommit, c: &Containers, key: &str) -> Option<String> {
    match doc.get(&c.meta, key) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// The document-wide multi-column layout: `(column_count, optional_gap_px)`.
/// Reads the `meta.columns` / `meta.column_gap` registers, defaulting to a single
/// column ([`DEFAULT_COLUMNS`]) with no gap when unset — so documents authored
/// before this setting existed read as single-column. A stored count of zero is
/// clamped up to one (a body always has at least one column).
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

/// The document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`), read from
/// the `meta.language` register. `None` for documents that declare no default language
/// (authored before this setting existed, or never set) — the consumer then falls back
/// to its own locale default. Round-trips through the `.odt` default-paragraph-style
/// `fo:language`/`fo:country` and the `.docx` `docDefaults` `w:lang`.
pub fn language(doc: &AutoCommit, c: &Containers) -> Option<String> {
    match doc.get(&c.meta, META_LANGUAGE) {
        Ok(Some((v, _))) => v.to_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
        _ => None,
    }
}

/// Resolve the document-properties sub-map under `meta` (`meta.props`), if it
/// exists. Created lazily on the first [`DocMut::set_doc_prop`], so documents
/// authored before the props map existed simply read as having no properties.
fn props_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    match doc.get(&c.meta, KEY_PROPS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Read a single document property (`title`/`author`/… — see [`DOC_PROP_KEYS`]),
/// or any other string property name a codec stashed. `None` if unset.
pub fn doc_prop(doc: &AutoCommit, c: &Containers, name: &str) -> Option<String> {
    let props = props_obj(doc, c)?;
    match doc.get(&props, name) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// All document properties (`name → value`) as a sorted map. Empty for documents
/// with no properties set.
pub fn doc_props(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(props) = props_obj(doc, c) {
        for k in doc.keys(&props) {
            if let Ok(Some((v, _))) = doc.get(&props, &k) {
                if let Some(s) = v.to_str() {
                    out.insert(k, s.to_string());
                }
            }
        }
    }
    out
}

pub fn block_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    match doc.get(&obj, BLOCK_KIND) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
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
                .map(|m| MarkSpan { start: m.start, end: m.end, name: m.name().to_string(), value: m.value().into() })
                .collect()
        })
        .unwrap_or_default();
    // Canonical order so two replicas that merged in different orders produce
    // byte-identical materializations (mark *set* is order-independent). The
    // value is part of the key so concurrent *valued* marks (fontSize/link/color)
    // that resolve to distinct spans still sort deterministically.
    spans.sort_by(|a, b| (a.start, a.end, &a.name, &a.value).cmp(&(b.start, b.end, &b.name, &b.value)));
    spans
}

pub fn marks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<MarkSpan> {
    match block_obj(doc, &c.blocks, id) {
        Some(obj) => marks_on(doc, &obj),
        None => vec![],
    }
}

/// Whether a mark named `name` fully covers `[start, end)` of block `id`.
pub fn has_mark_over(doc: &AutoCommit, c: &Containers, id: &str, start: usize, end: usize, name: &str) -> bool {
    marks(doc, c, id)
        .iter()
        .any(|m| m.name == name && m.start <= start && m.end >= end)
}

/// All preserved parts (the lossless preservation layer — verbatim bytes a codec
/// stashed for content it doesn't model), as `(key, bytes)`. The bytes are
/// reconstructed exactly (byte-for-byte) from however the part is stored — either
/// the legacy single-`Bytes` scalar form, or the per-atom segmented `Map` form
/// (see [`PRESERVED_SEGMENTS`]) that splits a part into independently-mergeable
/// atoms. Reconstruction concatenates the segments in their stored order, so a
/// part round-trips identically regardless of its storage granularity.
pub fn preserved_parts(doc: &AutoCommit, c: &Containers) -> Vec<(String, Vec<u8>)> {
    doc.keys(&c.preserved)
        .filter_map(|k| preserved_part(doc, c, &k).map(|b| (k, b)))
        .collect()
}

/// Reconstruct one preserved part's verbatim bytes by key, transparently handling
/// both the legacy `Bytes`-scalar form and the per-atom segmented `Map` form.
/// Concatenating the ordered segments reproduces the original part byte-for-byte.
pub fn preserved_part(doc: &AutoCommit, c: &Containers, key: &str) -> Option<Vec<u8>> {
    match doc.get(&c.preserved, key) {
        // Per-atom segmented form: a `Map<segment_id, {ord: Bytes, bytes: Bytes}>`.
        // Segments sort by their `ord` fractional key, then concatenate. This is
        // the CRDT-merge-friendly form: each segment is an independent automerge
        // object, so two replicas editing DIFFERENT atoms of the same part merge
        // (only same-atom edits remain last-writer-wins).
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
            // Sort by fractional order key, then by segment id as a deterministic
            // tiebreaker (so two segments that somehow share an order key still
            // produce a stable, replica-independent concatenation).
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

/// The per-atom segment ids of a preserved part, in source order, paired with each
/// atom's verbatim bytes. Empty for a part stored in the legacy single-blob form
/// (it has no addressable atoms — call [`preserved_part`] for its whole bytes).
/// Lets a codec or test address an individual atom by its stable id.
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

/// A stable, replica-independent id for a preserved atom: its source-order index
/// plus a short content hash. The index disambiguates two byte-identical atoms
/// (so a part with repeated identical fragments keeps one segment per occurrence);
/// the content hash keeps the id meaningful and stable across re-imports of the
/// same source. The same source bytes split into the same ids on every replica,
/// so the same atom maps to ONE automerge object and converges.
fn preserved_atom_id(idx: usize, atom: &[u8]) -> String {
    let h = blake3::hash(atom).to_hex();
    format!("{idx:06}-{}", &h.as_str()[..16])
}

/// Split a preserved part into independently-mergeable atoms whose concatenation
/// is the original bytes verbatim. For an XML part this is the sequence of
/// top-level fragments — each top-level child *element* (a `<style:style …>…`, an
/// `<office:automatic-styles>…`, etc.) is its own atom, and the inter-element text
/// (the XML declaration, whitespace, the root open/close tags) rides on adjacent
/// atoms — so two replicas modeling/modifying different top-level elements touch
/// different atoms. A non-XML or unparseable part stays a single whole-part atom
/// (still byte-faithful, just coarse-grained). The split NEVER changes the bytes:
/// the atoms always concatenate back to `bytes` exactly.
pub fn split_preserved_atoms(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Only XML parts (`<?xml` prologue or a leading `<` tag) get split; everything
    // else (images, the mimetype literal, binary settings) is one atom.
    let looks_xml = {
        let head: &[u8] = &bytes[..bytes.len().min(64)];
        let trimmed = head.iter().position(|b| !b.is_ascii_whitespace()).map(|p| &head[p..]).unwrap_or(head);
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

/// One parsed XML element span: its nesting `depth` (root = 0) and the byte offset
/// `start` of its opening `<`. (The end offset isn't needed for boundary slicing.)
struct ElemSpan {
    depth: usize,
    start: usize,
}

/// Parse the depth + start offset of every element in `bytes`. Returns `None` on
/// any parse error or imbalance (caller then keeps the part as a single atom —
/// never corrupts it).
fn parse_elem_spans(bytes: &[u8]) -> Option<Vec<ElemSpan>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
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
                spans.push(ElemSpan { depth, start: before });
                depth += 1;
            }
            Ok(Event::Empty(_)) => {
                spans.push(ElemSpan { depth, start: before });
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

/// Split XML bytes into per-element atoms, descending through single-child wrapper
/// layers so the split lands at the depth where the document actually *branches*
/// (the first element with 2+ element children). E.g. for
/// `<doc><styles><a/><b/></styles></doc>` the atoms are the prologue+`<doc><styles>`
/// open, `<a/>`, then `<b/></styles></doc>` — so `a` and `b` (the independent
/// unmodeled atoms) live in separate, independently-mergeable segments. Atoms
/// always concatenate back to `bytes` byte-for-byte; returns `None` if no useful
/// branch point exists or on any parse error (caller keeps the part whole).
fn split_xml_top_level(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let spans = parse_elem_spans(bytes)?;
    if spans.is_empty() {
        return None;
    }
    // Find the shallowest depth that branches into 2+ sibling elements *under a
    // single shared parent* — that's where the independent unmodeled atoms live.
    // We descend single-child wrapper layers transparently: a depth is only a valid
    // split point if its parent depth held exactly one element (so all the siblings
    // we'd split share that one parent — a clean, predictable boundary). Depth 0 is
    // special-cased: its "parent" is the (single, implicit) document root.
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
        // Split at the boundaries between these sibling elements. The leading atom
        // is everything before the first sibling (prologue + ancestor open tags);
        // each subsequent sibling's start is a boundary; the final atom runs to EOF
        // (last sibling + ancestor close tags + trailing text).
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
        let total: usize = atoms.iter().map(|a| a.len()).sum();
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
    DocSnapshot {
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
    }
}

// ---- styles container ------------------------------------------------------

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

/// Style ids defined in the document's styles container (e.g. `body`,
/// `heading1`, or an imported ODF named-style name).
pub fn style_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    doc.keys(&c.styles)
        .filter(|id| matches!(doc.get(&c.styles, id), Ok(Some((Value::Object(ObjType::Map), _)))))
        .collect()
}

/// The scalar properties of a single style (`name`/`font`/`size`/… → value), or
/// an empty map if the style id is unknown.
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
            }
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
    BlockView { id: id.into(), kind, text, marks, attrs, children }
}

// ---- borrowed mutable view -------------------------------------------------

/// A mutable view of a canonical document living in a borrowed `AutoCommit`.
/// Used by the editor (over `ui::State.doc`) and by codecs.
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

impl<'a> DocRef<'a> {
    pub fn materialize(&self) -> DocSnapshot {
        materialize(self.doc, self.c)
    }
}

impl<'a> DocMut<'a> {
    /// Resolve-or-create a region's block container. The body always exists;
    /// header/footer maps are created lazily on first write (so older documents
    /// migrate transparently) and cached back into `Containers` for later reads.
    /// Takes `&Containers` by value-copy of the relevant id so this doesn't need a
    /// mutable borrow of `self.c` (which is shared `&`).
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
        self.doc.put_object(&self.c.root, key, ObjType::Map).unwrap()
    }

    fn order_after_in(&self, container: &ObjId, after: Option<&str>) -> Vec<u8> {
        let doc: &AutoCommit = &*self.doc;
        let ordered = ordered_keys(doc, container);
        let pos = match after {
            None => 0,
            Some(id) => ordered
                .iter()
                .position(|x| x == id)
                .map(|i| i + 1)
                .unwrap_or(ordered.len()),
        };
        let lo = if pos == 0 { None } else { block_order(doc, container, &ordered[pos - 1]) };
        let hi = if pos >= ordered.len() { None } else { block_order(doc, container, &ordered[pos]) };
        crate::order::key_between(lo.as_deref(), hi.as_deref())
    }

    fn order_after(&self, after: Option<&str>) -> Vec<u8> {
        self.order_after_in(&self.c.blocks, after)
    }

    // NOTE: DocMut does NOT commit the automerge transaction — the host owns the
    // transaction boundary. The owned `Document` commits per op (for fine-grained
    // history); a host like `ui::State` commits once per user action (so its
    // snapshot-based undo records each action). Reads always observe pending ops.

    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        self.insert_block_in(Region::Body, after, kind)
    }

    /// Insert a block into a page region (`Body`/`Header`/`Footer`). Header/footer
    /// containers are created lazily on first insert. Block ids are minted from the
    /// same per-replica generator as the body, so they never collide across
    /// regions.
    pub fn insert_block_in(&mut self, region: Region, after: Option<&str>, kind: &str) -> String {
        let container = self.region_container(region);
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
        let obj = self.doc.put_object(&container, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
        id
    }

    /// Splice text into a block living in `region`.
    pub fn splice_text_in(&mut self, region: Region, block_id: &str, idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    /// Add an inline mark over a block living in `region`.
    pub fn add_mark_in(&mut self, region: Region, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<ScalarValue>) {
        if let Some(tid) = self.region_text_obj(region, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Set a scalar attribute on a block living in `region`.
    pub fn set_attr_in(&mut self, region: Region, block_id: &str, key: &str, value: impl Into<ScalarValue>) {
        let Some(container) = region_obj(self.doc, self.c, region) else { return };
        let Some(obj) = block_obj(self.doc, &container, block_id) else { return };
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
        self.add_mark_in(region, block_id, idx, idx + 1, MARK_FIELD, FIELD_PAGE_NUMBER);
    }

    // ---- footnotes / endnotes ---------------------------------------------

    /// Resolve-or-create the `notes` root container (created lazily on the first
    /// note insert so older documents migrate transparently).
    fn notes_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.notes.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_NOTES) {
            return obj;
        }
        self.doc.put_object(&self.c.root, KEY_NOTES, ObjType::Map).unwrap()
    }

    /// Create a new note (footnote/endnote) with an empty block sequence and return
    /// its freshly-minted id. The note's content is then filled via
    /// [`Self::insert_note_block`] + [`Self::splice_note_text`]. The id is minted
    /// from the same per-replica generator as body/region blocks, so it never
    /// collides with them.
    pub fn insert_note(&mut self, kind: NoteKind) -> String {
        let notes = self.notes_container();
        let id = self.ids.mint();
        let note = self.doc.put_object(&notes, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&note, NOTE_KIND, kind.as_str()).unwrap();
        self.doc.put_object(&note, NOTE_BLOCKS, ObjType::Map).unwrap();
        id
    }

    /// Place a note reference marker at `idx` in a body block: splice the
    /// placeholder character and cover it with a `note-ref` mark valued with the
    /// note id. The marker then merges with concurrent text edits and round-trips
    /// through the codecs (inverse of import, which re-creates both the note and
    /// its marker).
    pub fn insert_note_ref(&mut self, block_id: &str, idx: usize, note_id: &str) {
        self.splice_text(block_id, idx, 0, &FIELD_PLACEHOLDER.to_string());
        self.add_mark(block_id, idx, idx + 1, MARK_NOTE_REF, note_id);
    }

    /// Convenience: create a footnote, place its reference marker at `idx` in
    /// `block_id`, and return the new note id (its content is filled separately).
    pub fn insert_footnote(&mut self, block_id: &str, idx: usize) -> String {
        let id = self.insert_note(NoteKind::Footnote);
        self.insert_note_ref(block_id, idx, &id);
        id
    }

    /// Convenience: create an endnote, place its reference marker at `idx` in
    /// `block_id`, and return the new note id.
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

    /// Insert a block into a note's content (created lazily); returns its id, or
    /// `None` if the note id is unknown.
    pub fn insert_note_block(&mut self, note_id: &str, after: Option<&str>, kind: &str) -> Option<String> {
        let container = self.note_blocks_container(note_id)?;
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
        let obj = self.doc.put_object(&container, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
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
    pub fn splice_note_text(&mut self, note_id: &str, block_id: &str, idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    /// Add an inline mark over a note's block text.
    pub fn add_note_mark(&mut self, note_id: &str, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<ScalarValue>) {
        if let Some(tid) = self.note_text_obj(note_id, block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    // ---- comments / annotations -------------------------------------------

    /// Resolve-or-create the `comments` root container (created lazily on the first
    /// comment insert so older documents migrate transparently).
    fn comments_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.comments.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_COMMENTS) {
            return obj;
        }
        self.doc.put_object(&self.c.root, KEY_COMMENTS, ObjType::Map).unwrap()
    }

    /// Create a new comment with an `author`, a `date` string, and an empty block
    /// sequence; returns its freshly-minted id. The id is minted from the same
    /// per-replica generator as body/region/note blocks, so it never collides. The
    /// comment's content is then filled via [`Self::insert_comment_block`] +
    /// [`Self::splice_comment_text`]; the body anchor is placed via
    /// [`Self::add_comment_mark`].
    pub fn insert_comment(&mut self, author: &str, date: &str) -> String {
        let comments = self.comments_container();
        let id = self.ids.mint();
        let comment = self.doc.put_object(&comments, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&comment, COMMENT_AUTHOR, author).unwrap();
        self.doc.put(&comment, COMMENT_DATE, date).unwrap();
        self.doc.put_object(&comment, COMMENT_BLOCKS, ObjType::Map).unwrap();
        id
    }

    /// Anchor a comment to the body range `[start, end)` of `block_id`: cover it with
    /// a `comment` mark valued with the comment id. The mark merges with concurrent
    /// text edits and round-trips through the codecs.
    pub fn add_comment_mark(&mut self, block_id: &str, start: usize, end: usize, comment_id: &str) {
        self.add_mark(block_id, start, end, MARK_COMMENT, comment_id);
    }

    /// Convenience: create a comment authored by `author` with body `text` (a single
    /// paragraph), anchor it to the body range `[start, end)` of `block_id`, and
    /// return the new comment id. `date` is left empty (set it via a fuller path if a
    /// timestamp is desired). For multi-paragraph content, use [`Self::insert_comment`]
    /// + [`Self::insert_comment_block`] + [`Self::splice_comment_text`] directly.
    pub fn add_comment(&mut self, block_id: &str, start: usize, end: usize, author: &str, text: &str) -> String {
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
            _ => self.doc.put_object(&comment, COMMENT_BLOCKS, ObjType::Map).ok(),
        }
    }

    /// Insert a block into a comment's content (created lazily); returns its id, or
    /// `None` if the comment id is unknown.
    pub fn insert_comment_block(&mut self, comment_id: &str, after: Option<&str>, kind: &str) -> Option<String> {
        let container = self.comment_blocks_container(comment_id)?;
        let key = self.order_after_in(&container, after);
        let id = self.ids.mint();
        let obj = self.doc.put_object(&container, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
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
    pub fn splice_comment_text(&mut self, comment_id: &str, block_id: &str, idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.comment_text_obj(comment_id, block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    // ---- tracked changes / revision marks ---------------------------------

    /// Resolve-or-create the `changes` root container (created lazily on the first
    /// tracked change so older documents migrate transparently).
    fn changes_container(&mut self) -> ObjId {
        if let Some(obj) = self.c.changes.clone() {
            return obj;
        }
        if let Some(obj) = opt_map_at(self.doc, &self.c.root, KEY_CHANGES) {
            return obj;
        }
        self.doc.put_object(&self.c.root, KEY_CHANGES, ObjType::Map).unwrap()
    }

    /// Create a tracked-change record of `kind` with an `author` + `date`; returns its
    /// freshly-minted id. The id is minted from the same per-replica generator as
    /// body/region/note/comment ids, so it never collides. The body anchor is placed
    /// separately (via [`Self::add_change_mark`]) — callers usually go through the
    /// [`Self::track_insert`]/[`Self::track_delete`] convenience methods.
    pub fn insert_change(&mut self, kind: crate::snapshot::ChangeKind, author: &str, date: &str) -> String {
        let changes = self.changes_container();
        let id = self.ids.mint();
        let change = self.doc.put_object(&changes, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&change, CHANGE_KIND, kind.as_str()).unwrap();
        self.doc.put(&change, CHANGE_AUTHOR, author).unwrap();
        self.doc.put(&change, CHANGE_DATE, date).unwrap();
        id
    }

    /// Anchor a tracked change to the body range `[start, end)` of `block_id`: cover it
    /// with the `ins`/`del` mark matching the change's kind, valued with the change id.
    /// The mark merges with concurrent text edits and round-trips through the codecs.
    pub fn add_change_mark(&mut self, kind: crate::snapshot::ChangeKind, block_id: &str, start: usize, end: usize, change_id: &str) {
        self.add_mark(block_id, start, end, change_mark_name(kind), change_id);
    }

    /// Track a **insertion**: splice `text` into `block_id` at `char_idx`, then flag the
    /// inserted range with an `ins` mark valued with a fresh change id (author/date in
    /// the `changes` container). Returns the new change id. The text is real content —
    /// accept keeps it (dropping the mark), reject removes it.
    pub fn track_insert(&mut self, block_id: &str, char_idx: usize, text: &str, author: &str, date: &str) -> String {
        let id = self.insert_change(crate::snapshot::ChangeKind::Ins, author, date);
        let n = text.chars().count();
        self.splice_text(block_id, char_idx, 0, text);
        if n > 0 {
            self.add_change_mark(crate::snapshot::ChangeKind::Ins, block_id, char_idx, char_idx + n, &id);
        }
        id
    }

    /// Track a **deletion**: flag the existing body range `[start, end)` of `block_id`
    /// with a `del` mark valued with a fresh change id (author/date in the `changes`
    /// container). The text is *retained* (struck, not removed) so reviewers can see it;
    /// accept removes it, reject keeps it (dropping the mark). Returns the new change id.
    pub fn track_delete(&mut self, block_id: &str, start: usize, end: usize, author: &str, date: &str) -> String {
        let id = self.insert_change(crate::snapshot::ChangeKind::Del, author, date);
        if end > start {
            self.add_change_mark(crate::snapshot::ChangeKind::Del, block_id, start, end, &id);
        }
        id
    }

    /// Accept a tracked change, applying it permanently:
    ///   * an **insertion** keeps the inserted text and drops its `ins` mark;
    ///   * a **deletion** removes the struck text (and, with it, its `del` mark).
    /// The change record is removed from the `changes` container either way. A no-op for
    /// an unknown id.
    pub fn accept_change(&mut self, change_id: &str) {
        let Some(kind) = change_kind(self.doc, self.c, change_id) else { return };
        let range = change_range(self.doc, self.c, change_id);
        match kind {
            crate::snapshot::ChangeKind::Ins => {
                // Keep the text; just remove the revision mark over its range.
                if let Some((b, start, end)) = range {
                    if end > start {
                        self.remove_mark(&b, start, end, MARK_INS);
                    }
                }
            }
            crate::snapshot::ChangeKind::Del => {
                // Remove the struck text (the `del` mark goes with it).
                if let Some((b, start, end)) = range {
                    if end > start {
                        self.splice_text(&b, start, (end - start) as isize, "");
                    }
                }
            }
        }
        self.remove_change_record(change_id);
    }

    /// Reject a tracked change, reverting it (the inverse of [`Self::accept_change`]):
    ///   * an **insertion** removes the inserted text (and its `ins` mark);
    ///   * a **deletion** keeps the struck text and drops its `del` mark.
    /// The change record is removed from the `changes` container either way. A no-op for
    /// an unknown id.
    pub fn reject_change(&mut self, change_id: &str) {
        let Some(kind) = change_kind(self.doc, self.c, change_id) else { return };
        let range = change_range(self.doc, self.c, change_id);
        match kind {
            crate::snapshot::ChangeKind::Ins => {
                // Undo the insertion: delete the inserted text.
                if let Some((b, start, end)) = range {
                    if end > start {
                        self.splice_text(&b, start, (end - start) as isize, "");
                    }
                }
            }
            crate::snapshot::ChangeKind::Del => {
                // Keep the text; just drop the deletion mark over its range.
                if let Some((b, start, end)) = range {
                    if end > start {
                        self.remove_mark(&b, start, end, MARK_DEL);
                    }
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

    /// Anchor a **bookmark** (named anchor) over the body range `[start, end)` of
    /// `block_id`: cover it with a `bookmark` mark valued with `name`. The mark merges
    /// with concurrent text edits and round-trips through the codecs. A zero-width
    /// (point) bookmark is widened to one character when the block has text after the
    /// anchor (a mark needs a non-empty range to live on); an end-of-block point sits
    /// over the last character.
    pub fn add_bookmark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        let (mut s, mut e) = (start, end);
        if e <= s {
            // Widen a point bookmark to one character so the mark has somewhere to live.
            let blen = crate::model::block_text(self.doc, self.c, block_id).map(|t| t.chars().count()).unwrap_or(0);
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

    /// Anchor a **cross-reference** (internal link) over the body range `[start, end)`
    /// of `block_id`: cover it with a `link` mark valued `#<target>`, where `target` is
    /// a bookmark name or a heading's derived id (see [`heading_ref_id`]). The leading
    /// `#` distinguishes an internal reference from an external URL. A bare target that
    /// already begins with `#` is used verbatim. Round-trips through the codecs.
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
        let Some(container) = region_obj(self.doc, self.c, region) else { return };
        for id in ordered_keys(self.doc, &container) {
            let _ = self.doc.delete(&container, &id);
        }
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
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        let key = self.order_after(after);
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, id) {
            self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
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

    pub fn add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<ScalarValue>) {
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Set a scalar attribute (e.g. heading `level`, list `ordered`) in the
    /// block's `attrs` map.
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

    /// Set the document-wide multi-column layout: `count` newspaper-style columns
    /// for the body, with an optional `gap` (px) between them. Each is its own
    /// scalar register under `meta`, so two replicas changing different aspects
    /// field-level merge. A `count` of one with no gap clears the setting (the
    /// document reads as single-column). Round-trips through `.odt`/`.docx`.
    pub fn set_columns(&mut self, count: u32, gap: Option<f64>) {
        let count = count.max(1);
        if count <= 1 {
            let _ = self.doc.delete(&self.c.meta, META_COLUMNS);
        } else {
            let _ = self.doc.put(&self.c.meta, META_COLUMNS, count as u64);
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

    /// Set the document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`).
    /// Stored as a single `Str` register under `meta`, so two replicas changing the
    /// document language field-level merge rather than clobbering each other. An empty
    /// `code` clears the setting (the document then declares no default language).
    /// Round-trips through `.odt`/`.docx`. Supports spell-check locale selection and
    /// accessibility/screen-reader language.
    pub fn set_language(&mut self, code: &str) {
        if code.is_empty() {
            let _ = self.doc.delete(&self.c.meta, META_LANGUAGE);
        } else {
            let _ = self.doc.put(&self.c.meta, META_LANGUAGE, code);
        }
    }

    /// Resolve-or-create the `meta.props` document-properties sub-map. Created
    /// lazily so documents authored before it existed migrate transparently.
    fn props_map(&mut self) -> ObjId {
        match self.doc.get(&self.c.meta, KEY_PROPS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => self.doc.put_object(&self.c.meta, KEY_PROPS, ObjType::Map).unwrap(),
        }
    }

    /// Set a document property (`title`/`author`/`subject`/`keywords`/`created`/
    /// `modified`/`generator` — see [`DOC_PROP_KEYS`]). Each property is its own
    /// scalar register in the `meta.props` map, so two replicas editing different
    /// properties field-level merge rather than clobbering each other. Round-trips
    /// through `.odt`/`.docx` metadata parts.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        let props = self.props_map();
        let _ = self.doc.put(&props, name, value);
    }

    /// Upsert a named style into the styles container. Each property is written
    /// as its own scalar register *into the existing sub-map* (not a fresh map),
    /// so two replicas editing different properties of the same style field-level
    /// merge rather than clobbering each other.
    pub fn set_style(&mut self, id: &str, props: &BTreeMap<String, AttrVal>) {
        let obj = match self.doc.get(&self.c.styles, id) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => self.doc.put_object(&self.c.styles, id, ObjType::Map).unwrap(),
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

    /// Stash a verbatim blob in the preservation layer (e.g. an unmodeled ODF
    /// part), keyed by `key`. Re-emitted unchanged on export.
    ///
    /// This is the *segmented* (Tier-4, CRDT-merge-friendly) entry point: `bytes`
    /// is split into independently-mergeable atoms (top-level XML child elements
    /// for an XML part, otherwise a single whole-part atom) and stored as a
    /// `Map<segment_id, {ord, bytes}>`. The same source part splits into the same
    /// stable ids on every replica, so two replicas that each modify a DIFFERENT
    /// unmodeled atom of the same part now merge cleanly instead of clobbering the
    /// whole part. Reconstruction via [`preserved_part`] reproduces `bytes`
    /// byte-for-byte. Same-atom concurrent edits remain last-writer-wins.
    pub fn set_preserved(&mut self, key: &str, bytes: &[u8]) {
        self.set_preserved_atoms(key, split_preserved_atoms(bytes));
    }

    /// Stash a preserved part already split into ordered atoms (verbatim byte
    /// slices that concatenate back to the whole part). Each atom becomes an
    /// independent automerge object keyed by a stable structural-index +
    /// content-hash id, so distinct atoms merge across replicas. Replaces any
    /// existing part under `key`.
    pub fn set_preserved_atoms(&mut self, key: &str, atoms: Vec<Vec<u8>>) {
        // Replace the part wholesale: a fresh map keyed by per-atom stable ids.
        let part = self.doc.put_object(&self.c.preserved, key, ObjType::Map).unwrap();
        for (idx, atom) in atoms.iter().enumerate() {
            let id = preserved_atom_id(idx, atom);
            let seg = self.doc.put_object(&part, id.as_str(), ObjType::Map).unwrap();
            // Order key: the atom's source position, big-endian, so segments sort
            // back into source order deterministically and replica-independently.
            let ord = (idx as u32).to_be_bytes().to_vec();
            let _ = self.doc.put(&seg, PRESERVED_SEG_ORD, ScalarValue::Bytes(ord));
            let _ = self.doc.put(&seg, PRESERVED_SEG_BYTES, ScalarValue::Bytes(atom.clone()));
        }
    }

    /// Overwrite a single preserved atom's verbatim bytes in-place, identified by
    /// its stable segment id, without disturbing the part's other atoms. Used by a
    /// codec that re-models one unmodeled region while leaving the rest untouched.
    /// No-op if the part isn't segmented or the segment id is absent.
    pub fn set_preserved_atom_bytes(&mut self, key: &str, segment_id: &str, bytes: &[u8]) {
        let part = match self.doc.get(&self.c.preserved, key) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => return,
        };
        if let Ok(Some((Value::Object(ObjType::Map), seg))) = self.doc.get(&part, segment_id) {
            let _ = self.doc.put(&seg, PRESERVED_SEG_BYTES, ScalarValue::Bytes(bytes.to_vec()));
        }
    }

    /// Store `bytes` in the content-addressed blob store and return its blake3
    /// hex hash (the key an `image` block's `blob` attr references). Idempotent —
    /// re-putting identical bytes hashes to the same key. Creates the blobs map
    /// lazily for documents opened under an older schema.
    pub fn put_blob(&mut self, bytes: &[u8]) -> String {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let blobs = match blobs_obj(self.doc, self.c) {
            Some(b) => b,
            None => self.doc.put_object(&self.c.root, KEY_BLOBS, ObjType::Map).unwrap(),
        };
        let _ = self.doc.put(&blobs, hash.as_str(), ScalarValue::Bytes(bytes.to_vec()));
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
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_style("body", &props("Body", 16.5, false));
            dm.set_style("h1", &props("Heading 1", 30.0, true));
        }

        let mut got = style_ids(&doc, &c);
        got.sort();
        assert_eq!(got, vec!["body".to_string(), "h1".to_string()]);

        let bp = style_props(&doc, &c, "body");
        assert_eq!(bp.get(STYLE_SIZE), Some(&AttrVal::Float(16.5)), "fractional size stored as F64");
        assert_eq!(bp.get(STYLE_NAME), Some(&AttrVal::Str("Body".into())));
        assert_eq!(bp.get(STYLE_BOLD), Some(&AttrVal::Bool(false)));

        // The snapshot exposes the full styles map.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.styles.len(), 2);
        assert_eq!(snap.styles["h1"].get(STYLE_BOLD), Some(&AttrVal::Bool(true)));
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
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_doc_prop(PROP_TITLE, "Hello");
            dm.set_doc_prop(PROP_AUTHOR, "Ada");
        }
        assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Hello"));
        assert_eq!(doc_prop(&doc, &c, PROP_AUTHOR).as_deref(), Some("Ada"));
        let all = doc_props(&doc, &c);
        assert_eq!(all.get("title").map(String::as_str), Some("Hello"));
        assert_eq!(all.get("author").map(String::as_str), Some("Ada"));
        // The snapshot exposes the properties map.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.properties.get("title").map(String::as_str), Some("Hello"));
        // Field-level upsert: setting one property leaves the other intact.
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_doc_prop(PROP_TITLE, "Changed");
        }
        assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Changed"));
        assert_eq!(doc_prop(&doc, &c, PROP_AUTHOR).as_deref(), Some("Ada"), "untouched prop preserved");
    }

    #[test]
    fn open_is_backward_compatible_without_props_map() {
        // A document whose meta map predates the props sub-map must open + read as
        // having no properties (the props map is created lazily on first write).
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        // `init` does not create meta.props; confirm reads degrade gracefully.
        assert!(doc_props(&doc, &c).is_empty(), "no props map → empty");
        assert_eq!(doc_prop(&doc, &c, PROP_GENERATOR), None);
        let bytes = doc.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(doc_props(&reloaded, &c2).is_empty(), "reload still opens with no props");
    }

    #[test]
    fn set_style_is_field_level_upsert() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_style("h1", &props("Heading 1", 30.0, true));
            // Touch only one property; the others must survive (field-level merge).
            let mut one = BTreeMap::new();
            one.insert(STYLE_SIZE.to_string(), AttrVal::Float(40.0));
            dm.set_style("h1", &one);
        }
        let h1 = style_props(&doc, &c, "h1");
        assert_eq!(h1.get(STYLE_SIZE), Some(&AttrVal::Float(40.0)), "updated property");
        assert_eq!(h1.get(STYLE_BOLD), Some(&AttrVal::Bool(true)), "untouched property preserved");
        assert_eq!(h1.get(STYLE_NAME), Some(&AttrVal::Str("Heading 1".into())));

        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.remove_style("h1");
        }
        assert!(style_ids(&doc, &c).is_empty(), "style removed");
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::schema::*;
    use automerge::{AutoCommit, ROOT};

    #[test]
    fn header_footer_are_backward_compatible_and_lazily_created() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        // A fresh doc has no header/footer maps and reads as empty.
        assert!(c.header.is_none() && c.footer.is_none(), "regions absent on a fresh doc");
        assert!(region_blocks_in_order(&doc, &c, Region::Header).is_empty());
        assert!(materialize_region(&doc, &c, Region::Footer).is_empty());
        // A doc saved with no regions must still open.
        let bytes = doc.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(c2.header.is_none() && c2.footer.is_none(), "regions still absent after reload");
    }

    #[test]
    fn header_footer_blocks_and_page_number_field() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (hid, fid);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            hid = dm.insert_block_in(Region::Header, None, "paragraph");
            dm.splice_text_in(Region::Header, &hid, 0, 0, "Title");
            fid = dm.insert_block_in(Region::Footer, None, "paragraph");
            dm.splice_text_in(Region::Footer, &fid, 0, 0, "Page ");
            dm.insert_page_number_field(Region::Footer, &fid, 5);
        }
        // Header has the title; body is unaffected.
        let header = materialize_region(&doc, &c, Region::Header);
        assert_eq!(header.len(), 1);
        assert_eq!(header[0].text, "Title");
        assert!(blocks_in_order(&doc, &c).is_empty(), "body stays empty");

        // Footer text carries the placeholder + a page-number field mark over it.
        let footer = materialize_region(&doc, &c, Region::Footer);
        assert_eq!(footer.len(), 1);
        assert_eq!(footer[0].text, format!("Page {}", FIELD_PLACEHOLDER));
        let field = footer[0].marks.iter().find(|m| m.name == MARK_FIELD).expect("page-number field mark present");
        assert_eq!((field.start, field.end), (5, 6), "field covers exactly the placeholder");
        assert_eq!(field.value, crate::document::MarkValue::Str(FIELD_PAGE_NUMBER.into()));

        // The snapshot surfaces both regions.
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

        // Backward-compat: a document authored before the notes map existed (no
        // root.notes) must still open and read as having no notes.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc; // simulate the old schema by deleting the eagerly-created map
        old.delete(&ROOT, KEY_NOTES).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(c2.notes.is_none(), "old-schema doc opens with no notes container");
        assert!(note_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn footnote_marker_and_note_content() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, nid, nb);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "see ref");
            nid = dm.insert_footnote(&pid, 4); // after "see "
            nb = dm.insert_note_block(&nid, None, "paragraph").unwrap();
            dm.splice_note_text(&nid, &nb, 0, 0, "the note");
        }
        // Body carries the placeholder + a note-ref mark over it valued with the id.
        let p = materialize_block(&doc, &c.blocks, &pid);
        assert_eq!(p.text, format!("see {}ref", FIELD_PLACEHOLDER), "placeholder spliced inline");
        let nref = p.marks.iter().find(|m| m.name == MARK_NOTE_REF).expect("note-ref mark present");
        assert_eq!((nref.start, nref.end), (4, 5));
        assert_eq!(nref.value, crate::document::MarkValue::Str(nid.clone()));
        // The note exists with kind + content.
        assert_eq!(note_kind(&doc, &c, &nid), Some(NoteKind::Footnote));
        let blocks = materialize_note_blocks(&doc, &c, &nid);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "the note");
        // The snapshot surfaces the note.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.notes.len(), 1);
        assert_eq!(snap.notes[0].kind, "footnote");
        assert_eq!(snap.notes[0].blocks[0].text, "the note");
    }

    #[test]
    fn seeded_id_gen_does_not_remint_note_ids() {
        // A generator seeded over a reloaded doc must skip past note + note-block ids,
        // otherwise it would re-mint (and overwrite) an existing note.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, nid);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
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
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids2 };
            nid2 = dm.insert_footnote(&pid, 0);
            let nb2 = dm.insert_note_block(&nid2, None, "paragraph").unwrap();
            dm.splice_note_text(&nid2, &nb2, 0, 0, "second");
        }
        assert_ne!(nid, nid2, "second note id must not collide with the first");
        let snap = materialize(&doc, &c);
        assert_eq!(snap.notes.len(), 2, "both notes survive (no overwrite): {:?}", snap.notes);
    }

    #[test]
    fn comments_container_is_empty_on_fresh_doc_and_backward_compatible() {
        use automerge::transaction::Transactable;
        // A fresh doc has an (empty) comments container — created eagerly so concurrent
        // comment adds converge on one shared object.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        assert!(c.comments.is_some(), "comments container present on a fresh doc");
        assert!(comment_ids(&doc, &c).is_empty(), "but empty");
        assert!(materialize_comments(&doc, &c).is_empty());

        // Backward-compat: a document authored before the comments map existed (no
        // root.comments) must still open and read as having no comments.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc;
        old.delete(&ROOT, KEY_COMMENTS).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(c2.comments.is_none(), "old-schema doc opens with no comments container");
        assert!(comment_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn comment_anchor_and_content() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, cid);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "please fix this word");
            // Comment over "this word" (chars 11..20).
            cid = dm.add_comment(&pid, 11, 20, "Ada", "note: fix");
        }
        // The body carries a `comment` mark over the anchored range, valued with the id.
        let p = materialize_block(&doc, &c.blocks, &pid);
        let cm = p.marks.iter().find(|m| m.name == MARK_COMMENT).expect("comment mark present");
        assert_eq!((cm.start, cm.end), (11, 20));
        assert_eq!(cm.value, crate::document::MarkValue::Str(cid.clone()));
        // The comment exists with author + content; range recovered from the mark.
        assert_eq!(comment_author(&doc, &c, &cid).as_deref(), Some("Ada"));
        let blocks = materialize_comment_blocks(&doc, &c, &cid);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "note: fix");
        // The snapshot surfaces the comment with its anchored range.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.comments.len(), 1);
        assert_eq!(snap.comments[0].author, "Ada");
        assert_eq!(snap.comments[0].blocks[0].text, "note: fix");
        assert_eq!(snap.comments[0].range, Some((pid.clone(), 11, 20)));
    }

    #[test]
    fn seeded_id_gen_does_not_remint_comment_ids() {
        // A generator seeded over a reloaded doc must skip past comment ids, otherwise
        // it would re-mint (and overwrite) an existing comment.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, cid);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "abcdef");
            cid = dm.add_comment(&pid, 0, 3, "A", "first");
        }
        let mut ids2 = seeded_id_gen(&doc, &c);
        let cid2;
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids2 };
            cid2 = dm.add_comment(&pid, 3, 6, "B", "second");
        }
        assert_ne!(cid, cid2, "second comment id must not collide with the first");
        let snap = materialize(&doc, &c);
        assert_eq!(snap.comments.len(), 2, "both comments survive (no overwrite): {:?}", snap.comments);
    }

    #[test]
    fn changes_container_is_empty_on_fresh_doc_and_backward_compatible() {
        use automerge::transaction::Transactable;
        // A fresh doc has an (empty) changes container — created eagerly so concurrent
        // change tracking converges on one shared object.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        assert!(c.changes.is_some(), "changes container present on a fresh doc");
        assert!(change_ids(&doc, &c).is_empty(), "but empty");
        assert!(materialize_changes(&doc, &c).is_empty());

        // Backward-compat: a document authored before the changes map existed (no
        // root.changes) must still open and read as having no changes.
        let mut old = AutoCommit::new();
        let oc = init(&mut old, &ROOT);
        let _ = oc;
        old.delete(&ROOT, KEY_CHANGES).unwrap();
        let bytes = old.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(c2.changes.is_none(), "old-schema doc opens with no changes container");
        assert!(change_ids(&reloaded, &c2).is_empty());
    }

    #[test]
    fn tracked_insert_and_delete_anchor_and_metadata() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, ins_id, del_id);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "hello world");
            // Track an insertion of " there" after "hello" (idx 5).
            ins_id = dm.track_insert(&pid, 5, " there", "Ada", "2021-01-02");
            // Track a deletion of "world" (now at the end). After the insert the text is
            // "hello there world"; "world" is chars 12..17.
            del_id = dm.track_delete(&pid, 12, 17, "Bob", "2021-03-04");
        }
        let p = materialize_block(&doc, &c.blocks, &pid);
        assert_eq!(p.text, "hello there world", "inserted text is real content, deletion retained");
        // The body carries an `ins` mark over the inserted range valued with the id.
        let im = p.marks.iter().find(|m| m.name == MARK_INS).expect("ins mark present");
        assert_eq!((im.start, im.end), (5, 11), "ins covers ' there'");
        assert_eq!(im.value, crate::document::MarkValue::Str(ins_id.clone()));
        // And a `del` mark over the retained-but-struck range.
        let dmrk = p.marks.iter().find(|m| m.name == MARK_DEL).expect("del mark present");
        assert_eq!((dmrk.start, dmrk.end), (12, 17), "del covers 'world'");
        assert_eq!(dmrk.value, crate::document::MarkValue::Str(del_id.clone()));
        // Change records carry kind + author + date.
        assert_eq!(change_kind(&doc, &c, &ins_id), Some(crate::snapshot::ChangeKind::Ins));
        assert_eq!(change_kind(&doc, &c, &del_id), Some(crate::snapshot::ChangeKind::Del));
        assert_eq!(change_author(&doc, &c, &ins_id).as_deref(), Some("Ada"));
        assert_eq!(change_author(&doc, &c, &del_id).as_deref(), Some("Bob"));
        assert_eq!(change_date(&doc, &c, &del_id).as_deref(), Some("2021-03-04"));
        // The snapshot surfaces both with their anchored ranges.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.changes.len(), 2);
        let ins = snap.changes.iter().find(|ch| ch.id == ins_id).unwrap();
        assert_eq!(ins.kind, crate::snapshot::ChangeKind::Ins);
        assert_eq!(ins.range, Some((pid.clone(), 5, 11)));
        let del = snap.changes.iter().find(|ch| ch.id == del_id).unwrap();
        assert_eq!(del.kind, crate::snapshot::ChangeKind::Del);
        assert_eq!(del.range, Some((pid.clone(), 12, 17)));
    }

    #[test]
    fn accept_insertion_clears_mark_keeps_text() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let pid;
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "ab");
            let id = dm.track_insert(&pid, 2, "cd", "A", "");
            dm.accept_change(&id);
        }
        let p = materialize_block(&doc, &c.blocks, &pid);
        assert_eq!(p.text, "abcd", "accepted insertion keeps the text");
        assert!(p.marks.iter().all(|m| m.name != MARK_INS), "ins mark dropped on accept");
        assert!(materialize_changes(&doc, &c).is_empty(), "change record removed on accept");
    }

    #[test]
    fn accept_deletion_removes_text() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let pid;
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "keep DROP end");
            let id = dm.track_delete(&pid, 5, 9, "A", ""); // "DROP"
            dm.accept_change(&id);
        }
        let p = materialize_block(&doc, &c.blocks, &pid);
        assert_eq!(p.text, "keep  end", "accepted deletion removes the struck text");
        assert!(p.marks.iter().all(|m| m.name != MARK_DEL), "del mark gone with the text");
        assert!(materialize_changes(&doc, &c).is_empty(), "change record removed on accept");
    }

    #[test]
    fn reject_inverts_accept() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let pid;
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "abYZ");
            // Reject an insertion → text removed.
            let ins = dm.track_insert(&pid, 4, "QQ", "A", "");
            dm.reject_change(&ins);
            // Reject a deletion → text kept, mark dropped.
            let del = dm.track_delete(&pid, 2, 4, "A", ""); // "YZ"
            dm.reject_change(&del);
        }
        let p = materialize_block(&doc, &c.blocks, &pid);
        assert_eq!(p.text, "abYZ", "reject-ins removed inserted text; reject-del kept struck text");
        assert!(p.marks.iter().all(|m| m.name != MARK_INS && m.name != MARK_DEL), "both marks dropped");
        assert!(materialize_changes(&doc, &c).is_empty(), "both records removed");
    }

    #[test]
    fn seeded_id_gen_does_not_remint_change_ids() {
        // A generator seeded over a reloaded doc must skip past change ids, otherwise it
        // would re-mint (and overwrite) an existing change.
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (pid, chid);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            pid = dm.insert_block(None, "paragraph");
            dm.splice_text(&pid, 0, 0, "abcdef");
            chid = dm.track_delete(&pid, 0, 3, "A", "");
        }
        let mut ids2 = seeded_id_gen(&doc, &c);
        let chid2;
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids2 };
            chid2 = dm.track_delete(&pid, 3, 6, "B", "");
        }
        assert_ne!(chid, chid2, "second change id must not collide with the first");
        let snap = materialize(&doc, &c);
        assert_eq!(snap.changes.len(), 2, "both changes survive (no overwrite): {:?}", snap.changes);
    }

    #[test]
    fn bookmark_anchor_cross_ref_and_snapshot() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (target, src);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            target = dm.insert_block(None, "paragraph");
            dm.splice_text(&target, 0, 0, "the intro section");
            dm.add_bookmark(&target, 4, 9, "intro"); // over "intro"
            src = dm.insert_block(Some(&target), "paragraph");
            dm.splice_text(&src, 0, 0, "see above");
            dm.add_cross_ref(&src, 4, 9, "intro"); // "above" → #intro
        }
        // The bookmarked block carries a `bookmark` mark valued with the name.
        let tb = materialize_block(&doc, &c.blocks, &target);
        let bm = tb.marks.iter().find(|m| m.name == MARK_BOOKMARK).expect("bookmark mark present");
        assert_eq!((bm.start, bm.end), (4, 9));
        assert_eq!(bm.value, crate::document::MarkValue::Str("intro".into()));
        // The cross-ref is a `link` mark valued `#intro`.
        let sb = materialize_block(&doc, &c.blocks, &src);
        let link = sb.marks.iter().find(|m| m.name == MARK_LINK).expect("cross-ref link mark present");
        assert_eq!((link.start, link.end), (4, 9));
        assert_eq!(link.value, crate::document::MarkValue::Str("#intro".into()));
        // The snapshot surfaces the bookmark registry.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.bookmarks.len(), 1);
        assert_eq!(snap.bookmarks[0].name, "intro");
        assert_eq!(snap.bookmarks[0].range, (target.clone(), 4, 9));
    }

    #[test]
    fn point_bookmark_is_widened_and_heading_ref_id_targets_heading() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let (h, p);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            h = dm.insert_block(None, "heading");
            dm.set_attr(&h, "level", 1i64);
            dm.splice_text(&h, 0, 0, "Title");
            p = dm.insert_block(Some(&h), "paragraph");
            dm.splice_text(&p, 0, 0, "abc");
            // A point bookmark at idx 1 (start == end) widens to one character.
            dm.add_bookmark(&p, 1, 1, "pt");
            // A cross-reference targeting the heading's derived id.
            dm.add_cross_ref(&p, 0, 3, &heading_ref_id(&h));
        }
        let snap = materialize(&doc, &c);
        let pt = snap.bookmarks.iter().find(|b| b.name == "pt").expect("point bookmark present");
        assert_eq!((pt.range.1, pt.range.2), (1, 2), "point bookmark widened to one char");
        let link = snap.blocks.iter().find(|b| b.text == "abc").unwrap().marks.iter().find(|m| m.name == MARK_LINK).unwrap().clone();
        assert_eq!(link.value, crate::document::MarkValue::Str(format!("#{}{}", HEADING_REF_PREFIX, h)));
    }

    #[test]
    fn bookmarks_converge_across_replicas() {
        // Two replicas each add a bookmark to the same document; after merge both
        // converge with both bookmarks present (CRDT-native — the mark is the source).
        let mut a = crate::Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "shared base text");
        let mut b = a.fork();
        a.add_bookmark(&p, 0, 6, "first"); // "shared"
        b.add_bookmark(&p, 7, 11, "second"); // "base"
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas converge with bookmarks");
        assert_eq!(a.materialize().bookmarks.len(), 2, "both bookmarks present after merge");
    }

    #[test]
    fn clear_region_empties_it() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let a = dm.insert_block_in(Region::Header, None, "paragraph");
            dm.splice_text_in(Region::Header, &a, 0, 0, "one");
            dm.clear_region(Region::Header);
        }
        assert!(materialize_region(&doc, &c, Region::Header).is_empty(), "header cleared");
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
        // Two depth-1 siblings → two atoms: [prologue + root-open + style A],
        // [style B + root-close]. (The first sibling rides with the leading
        // material; each *subsequent* sibling opens a new atom.)
        assert_eq!(atoms.len(), 2, "two style atoms: {atoms:?}");
        let joined: Vec<u8> = atoms.iter().flatten().copied().collect();
        assert_eq!(joined, TWO_STYLE_PART, "atoms must concatenate to the source byte-for-byte");
        // The two style elements live in distinct atoms.
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
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_preserved("odt/part/styles.xml", TWO_STYLE_PART);
        }
        // Stored as a segmented Map, yet reconstructs byte-for-byte.
        let got = preserved_part(&doc, &c, "odt/part/styles.xml").unwrap();
        assert_eq!(got, TWO_STYLE_PART, "segmented part re-emits verbatim");
        // And via the bulk accessor the export paths use.
        let parts = preserved_parts(&doc, &c);
        let p = parts.iter().find(|(k, _)| k == "odt/part/styles.xml").unwrap();
        assert_eq!(p.1, TWO_STYLE_PART);
        // It really is segmented (more than one addressable atom).
        assert!(preserved_atoms(&doc, &c, "odt/part/styles.xml").len() >= 2);
    }

    #[test]
    fn legacy_blob_form_still_reconstructs() {
        // Simulate a document written under the old single-`Bytes`-scalar schema.
        let (mut doc, c, _ids) = new_doc();
        doc.put(&c.preserved, "odt/part/settings.xml", ScalarValue::Bytes(TWO_STYLE_PART.to_vec()))
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
        // Base replica with the two-style part preserved.
        let (mut doc, c, mut ids) = new_doc();
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_preserved("odt/part/styles.xml", TWO_STYLE_PART);
        }
        doc.commit();

        // Fork into replica B (distinct actor); A and B share the same per-atom
        // object ids because they were created in the common ancestor.
        let mut doc_b = AutoCommit::load(&doc.save()).unwrap();
        doc_b.set_actor(automerge::ActorId::random());
        let c_b = open(&doc_b, &ROOT).unwrap();

        // Identify the two style atoms by their stable ids (same on both replicas).
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
            atoms_b.iter().any(|(id, _)| id == &id_alpha) && atoms_b.iter().any(|(id, _)| id == &id_bravo),
            "fork shares the same stable atom ids: A={atoms_a:?} B={atoms_b:?}"
        );

        // Replica A rewrites ONLY the alpha atom; replica B rewrites ONLY bravo.
        // Read-modify-write the atom (a targeted edit that keeps the atom's
        // framing — prologue/root-open on the first atom, close tags on the last),
        // mirroring how a codec would re-model one region in place.
        let edit_atom = |bytes: &[u8], from: &str, to: &str| -> Vec<u8> {
            String::from_utf8_lossy(bytes).replace(from, to).into_bytes()
        };
        let cur_alpha = atoms_a.iter().find(|(id, _)| id == &id_alpha).unwrap().1.clone();
        let new_alpha = edit_atom(&cur_alpha, "alpha", "ALPHA-EDITED");
        let cur_bravo = atoms_a.iter().find(|(id, _)| id == &id_bravo).unwrap().1.clone();
        let new_bravo = edit_atom(&cur_bravo, "bravo", "BRAVO-EDITED");
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_preserved_atom_bytes("odt/part/styles.xml", &id_alpha, &new_alpha);
        }
        doc.commit();
        {
            let mut ids_b = IdGen::new(doc_b.get_actor().to_hex_string());
            let mut dm_b = DocMut { doc: &mut doc_b, c: &c_b, ids: &mut ids_b };
            dm_b.set_preserved_atom_bytes("odt/part/styles.xml", &id_bravo, &new_bravo);
        }
        doc_b.commit();

        // Merge both directions via the low-level Automerge (mirroring
        // `Document::merge`, which sidesteps an automerge-0.8 AutoCommit::merge
        // patch-log panic). Reload each side from the merged bytes.
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
            assert!(txt.contains("ALPHA-EDITED"), "replica {who} kept A's edit: {txt}");
            assert!(txt.contains("BRAVO-EDITED"), "replica {who} kept B's edit: {txt}");
            // Source order is preserved (alpha before bravo) and the leading
            // declaration/root-open atom survives untouched.
            let ai = txt.find("ALPHA-EDITED").unwrap();
            let bi = txt.find("BRAVO-EDITED").unwrap();
            assert!(ai < bi, "replica {who} kept atom order");
            assert!(txt.contains("<office:styles"), "replica {who} kept the root open tag");
        }
        // Convergence: both replicas reconstruct identical bytes.
        assert_eq!(
            preserved_part(&doc, &c, "odt/part/styles.xml"),
            preserved_part(&doc_b, &c_b, "odt/part/styles.xml"),
            "replicas converge on identical reconstructed bytes"
        );
    }
}
