//! Rich text as a resource: the canonical document an editor, the palette,
//! agents and scripts all edit through one typed action vocabulary
//! ([`RichTextAction`]), and read through one incrementally maintained state
//! ([`RichTextState`]).
//!
//! A long document is not read whole. [`RichText::read`] reads the *spine* —
//! every paragraph's position and kind, the running header and footer, notes,
//! comments and tracked changes — and [`RichText::load`] reads the paragraphs
//! a view shows (a [`Demand`] of positions in the body). After a change,
//! [`RichText::refresh`] re-reads only the paragraphs it touched, so a
//! keystroke costs as much as its paragraph however long the document is.
//! The heading outline is a projection kept current the same way.

pub mod action;
pub mod block_actions;
pub mod block_edit;
pub mod block_markdown;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use automerge::{ObjId, ObjType, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_resource::{Delta, Demand, Doc, ModelError, Resource, SearchEntry, Touch};

use tilo_doc::model::{self, Containers, Region};
use tilo_doc::schema::{
    BLOCK_ATTRS, BLOCK_ORDER, BLOCK_TEXT, KEY_BLOBS, KEY_BLOCKS, KEY_CHANGES, KEY_COMMENTS,
    KEY_FOOTER, KEY_HEADER, KEY_META, KEY_NOTES, KEY_STYLES,
};
use tilo_doc::snapshot::{AttrVal, BlockView, ChangeView, CommentView, NoteView};

pub use action::{RICH_TEXT_ACTION_TYPE, RICH_TEXT_ACTION_TYPE_LEGACY, RichTextAction};

/// The styles every document has; they cannot be deleted.
pub const BUILTIN_STYLES: [&str; 6] = ["body", "heading1", "heading2", "heading3", "list", "quote"];

/// One block of a copied rich fragment — text plus its formatting (marks and
/// paragraph attributes), with mark offsets relative to the block's own text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct ClipBlock {
    pub kind: String,
    pub text: String,
    pub marks: Vec<tilo_doc::document::MarkSpan>,
    pub level: Option<i64>,
    pub ordered: Option<bool>,
    pub align: Option<String>,
    /// The block's other attributes (a checklist item's `checked`, a code
    /// line's `lang`), set as they are.
    #[serde(default)]
    pub attrs: std::collections::BTreeMap<String, ClipAttr>,
}

/// One block attribute a [`ClipBlock`] carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub enum ClipAttr {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// The rich-text resource. See the module docs.
pub struct RichText;

/// One entry of the heading outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub id: String,
    /// 1-based outline level.
    pub level: i64,
    pub text: String,
}

/// The body's paragraph order: every ordered block's key and id, sorted as
/// the document orders them (fractional key, then id).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyOrder {
    entries: Vec<(Arc<[u8]>, Arc<str>)>,
    keys: HashMap<Arc<str>, Arc<[u8]>>,
}

fn order_cmp(a: &(Arc<[u8]>, Arc<str>), key: &[u8], id: &str) -> std::cmp::Ordering {
    tilo_resource::order::bytes::cmp(&a.0, key).then_with(|| a.1.as_ref().cmp(id))
}

impl BodyOrder {
    fn from_spine(spine: Vec<model::SpineEntry>) -> Self {
        let mut order = Self::default();
        order.entries.reserve(spine.len());
        for entry in spine {
            let key: Arc<[u8]> = entry.order.into();
            let id: Arc<str> = entry.id.into();
            order.keys.insert(Arc::clone(&id), Arc::clone(&key));
            order.entries.push((key, id));
        }
        order
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The id at position `index`.
    pub fn id_at(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(|(_, id)| id.as_ref())
    }

    /// The ids of positions `range`, clamped to the body.
    pub fn ids(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &str> {
        let end = range.end.min(self.entries.len());
        let start = range.start.min(end);
        self.entries
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .map(|(_, id)| id.as_ref())
    }

    /// Every id, in order.
    pub fn all(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(_, id)| id.as_ref())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.keys.contains_key(id)
    }

    /// The position of `id`, found by its order key in `O(log n)`.
    pub fn position(&self, id: &str) -> Option<usize> {
        let key = self.keys.get(id)?;
        self.entries
            .binary_search_by(|probe| order_cmp(probe, key, id))
            .ok()
    }

    fn key(&self, id: &str) -> Option<&[u8]> {
        self.keys.get(id).map(AsRef::as_ref)
    }

    fn remove(&mut self, id: &str) {
        if let Some(position) = self.position(id) {
            self.entries.remove(position);
        }
        self.keys.remove(id);
    }

    fn insert(&mut self, id: &str, key: &[u8]) {
        self.remove(id);
        let at = self
            .entries
            .binary_search_by(|probe| order_cmp(probe, key, id))
            .unwrap_or_else(|at| at);
        let key: Arc<[u8]> = key.into();
        let id: Arc<str> = id.into();
        self.keys.insert(Arc::clone(&id), Arc::clone(&key));
        self.entries.insert(at, (key, id));
    }
}

/// How many changed paragraphs a state remembers for views catching up
/// ([`RichTextState::changed_since`]); a view further behind reads it all.
const CHANGE_LOG_LIMIT: usize = 4096;

/// A point in a state's history of body changes, for a view that presented
/// the body then to find what changed since ([`RichTextState::changed_since`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeCursor {
    read: u64,
    order: u64,
    at: u64,
}

/// The body paragraphs whose content changed, in the order their changes
/// were read, since the state was read whole.
#[derive(Clone, Debug, Default)]
struct ChangeLog {
    /// Which whole read this log continues; a new read starts a new one.
    read: u64,
    /// Bumped whenever paragraphs are added, removed, moved, or come into or
    /// out of view: positions shown before no longer hold.
    order: u64,
    /// The position of `changed[0]` in the whole history.
    base: u64,
    changed: Vec<String>,
}

impl ChangeLog {
    fn fresh() -> Self {
        static READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            read: READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ..Self::default()
        }
    }

    fn content(&mut self, id: &str) {
        if self.changed.len() >= CHANGE_LOG_LIMIT {
            let dropped = self.changed.len() / 2;
            self.changed.drain(..dropped);
            self.base = self
                .base
                .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
        }
        self.changed.push(id.to_owned());
    }

    fn order(&mut self) {
        self.order = self.order.wrapping_add(1);
    }

    fn cursor(&self) -> ChangeCursor {
        ChangeCursor {
            read: self.read,
            order: self.order,
            at: self
                .base
                .saturating_add(u64::try_from(self.changed.len()).unwrap_or(u64::MAX)),
        }
    }

    fn since(&self, cursor: ChangeCursor) -> Option<&[String]> {
        if cursor.read != self.read || cursor.order != self.order {
            return None;
        }
        let from = usize::try_from(cursor.at.checked_sub(self.base)?).ok()?;
        self.changed.get(from..)
    }
}

/// What views of a rich-text document read. Cheap to clone: the parts are
/// shared, and only the paragraphs on screen are held.
#[derive(Clone, Debug)]
pub struct RichTextState {
    containers: Containers,
    /// Where the rich text sits in its document: empty at the root, the
    /// path to the field's map when it is a field of another resource.
    root_path: tilo_resource::Path,
    body: Rc<BodyOrder>,
    /// Every heading, image and table cell, read whole: the outline and the
    /// document's objects are listed without reading its prose.
    featured: Rc<HashMap<String, Rc<BlockView>>>,
    loaded: HashMap<String, Rc<BlockView>>,
    /// Where the objects of loaded and featured paragraphs sit (the
    /// paragraph's map, its text, its attributes), so a change to one is
    /// placed without asking the document.
    objects: HashMap<ObjId, tilo_resource::Path>,
    /// The objects indexed for each paragraph, so forgetting one touches
    /// only its own.
    paragraph_objects: HashMap<String, Vec<ObjId>>,
    header: Rc<Vec<BlockView>>,
    footer: Rc<Vec<BlockView>>,
    notes: Rc<Vec<NoteView>>,
    comments: Rc<Vec<CommentView>>,
    changes: Rc<Vec<ChangeView>>,
    /// Named paragraph styles (`style id → property → value`). Small: one
    /// entry per style, not per paragraph.
    styles: Rc<BTreeMap<String, BTreeMap<String, AttrVal>>>,
    /// Every content address the document refers to (its images and
    /// preserved package parts); the bytes live in the caller's store.
    content: Rc<BTreeSet<String>>,
    /// The content address of the file the document was last read from or
    /// written to ([`action::FILE_DIGEST`]).
    file_digest: Option<Rc<str>>,
    /// Which body paragraphs changed, for views presenting only those.
    log: ChangeLog,
}

impl PartialEq for RichTextState {
    /// Equal content; the container ids are where it was read from.
    fn eq(&self, other: &Self) -> bool {
        self.body == other.body
            && self.featured == other.featured
            && self.loaded == other.loaded
            && self.header == other.header
            && self.footer == other.footer
            && self.notes == other.notes
            && self.comments == other.comments
            && self.changes == other.changes
            && self.styles == other.styles
            && self.content == other.content
            && self.file_digest == other.file_digest
    }
}

/// The id prefix of what a new document starts with: no device's actor, so
/// every device mints the same ids.
const GENESIS_IDS: &str = "genesis";

/// The content address of the file the document under `c` was last read
/// from or written to.
fn file_digest(doc: &Doc, c: &Containers) -> Option<Rc<str>> {
    model::meta_str(doc, c, action::FILE_DIGEST).map(Rc::from)
}

impl RichTextState {
    /// Where the document's containers live.
    pub fn containers(&self) -> &Containers {
        &self.containers
    }

    /// Where the history of body changes is now, for [`Self::changed_since`].
    pub fn change_cursor(&self) -> ChangeCursor {
        self.log.cursor()
    }

    /// The body paragraphs whose content changed since `cursor`, some
    /// possibly more than once, while the body kept its order and the same
    /// paragraphs were read. `None` when that cannot be told (the body was
    /// reordered, read whole again, or the view is too far behind): read it
    /// all.
    pub fn changed_since(&self, cursor: ChangeCursor) -> Option<&[String]> {
        self.log.since(cursor)
    }

    /// The content address of the file the document was last read from or
    /// written to, so an open can tell that the file changed elsewhere.
    pub fn file_digest(&self) -> Option<&str> {
        self.file_digest.as_deref()
    }

    /// The body's paragraph order. Shared: hold it across frames freely.
    pub fn body(&self) -> &Rc<BodyOrder> {
        &self.body
    }

    /// A body paragraph, when it has been loaded ([`RichText::load`]). The
    /// same `Rc` until the paragraph changes, so a view can tell a changed
    /// paragraph by pointer.
    pub fn block(&self, id: &str) -> Option<&Rc<BlockView>> {
        self.loaded.get(id)
    }

    /// The loaded paragraphs of positions `range`, in order; unloaded ones
    /// are skipped.
    pub fn blocks(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &Rc<BlockView>> {
        self.body.ids(range).filter_map(|id| self.loaded.get(id))
    }

    /// The heading outline, in document order.
    pub fn headings(&self) -> Vec<Heading> {
        self.featured("heading")
            .iter()
            .filter_map(|view| Self::heading_of(view))
            .collect()
    }

    /// Every block of `kind` — `heading`, `image` or `cell` — in document
    /// order.
    pub fn featured(&self, kind: &str) -> Vec<Rc<BlockView>> {
        let mut blocks: Vec<(usize, &Rc<BlockView>)> = self
            .featured
            .values()
            .filter(|view| view.kind == kind)
            .filter_map(|view| Some((self.body.position(&view.id)?, view)))
            .collect();
        blocks.sort_by_key(|(position, _)| *position);
        blocks
            .into_iter()
            .map(|(_, view)| Rc::clone(view))
            .collect()
    }

    pub fn header(&self) -> &Rc<Vec<BlockView>> {
        &self.header
    }

    pub fn footer(&self) -> &Rc<Vec<BlockView>> {
        &self.footer
    }

    pub fn notes(&self) -> &Rc<Vec<NoteView>> {
        &self.notes
    }

    pub fn comments(&self) -> &Rc<Vec<CommentView>> {
        &self.comments
    }

    pub fn changes(&self) -> &Rc<Vec<ChangeView>> {
        &self.changes
    }

    /// Named paragraph styles defined in the document.
    pub fn styles(&self) -> &Rc<BTreeMap<String, BTreeMap<String, AttrVal>>> {
        &self.styles
    }

    /// Every content address the document refers to, whether or not the
    /// caller's store holds the bytes yet.
    pub fn content(&self) -> &Rc<BTreeSet<String>> {
        &self.content
    }

    /// Kinds the state keeps whole, however long the document.
    const FEATURED: [&'static str; 3] = ["heading", "image", "cell"];

    fn is_featured(kind: &str) -> bool {
        Self::FEATURED.contains(&kind)
    }

    fn heading_of(view: &BlockView) -> Option<Heading> {
        (view.kind == "heading").then(|| Heading {
            id: view.id.clone(),
            level: match view.attrs.get("level") {
                Some(AttrVal::Int(level)) => (*level).max(1),
                _ => 1,
            },
            text: view.text.clone(),
        })
    }
}

fn containers_of(doc: &Doc, root: &ObjId) -> Result<Containers, ModelError> {
    model::open(doc, root).map_err(|error| ModelError::invalid(error.to_string()))
}

/// The path from the document's root to `obj`, root first.
fn path_to(doc: &Doc, obj: &ObjId) -> tilo_resource::Path {
    let mut path: tilo_resource::Path = doc
        .parents(obj)
        .map(|parents| parents.map(|parent| (parent.obj, parent.prop)).collect())
        .unwrap_or_default();
    path.reverse();
    path
}

fn block_obj(doc: &Doc, blocks: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(blocks, id) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
        _ => None,
    }
}

/// The objects of body paragraph `id` and the path to each.
fn paragraph_objects(
    doc: &Doc,
    c: &Containers,
    root_path: &[(ObjId, automerge::Prop)],
    id: &str,
) -> Vec<(ObjId, tilo_resource::Path)> {
    use automerge::Prop;
    let Some(block) = block_obj(doc, &c.blocks, id) else {
        return Vec::new();
    };
    let mut to_block = root_path.to_vec();
    to_block.push((c.root.clone(), Prop::Map(KEY_BLOCKS.into())));
    to_block.push((c.blocks.clone(), Prop::Map(id.into())));
    let mut objects = vec![(block.clone(), to_block.clone())];
    for key in [BLOCK_TEXT, BLOCK_ATTRS] {
        if let Ok(Some((Value::Object(_), child))) = doc.get(&block, key) {
            let mut path = to_block.clone();
            path.push((block.clone(), Prop::Map(key.into())));
            objects.push((child, path));
        }
    }
    objects
}

impl RichTextState {
    /// Index where paragraph `id`'s objects sit, dropping what it had.
    fn index_paragraph(&mut self, doc: &Doc, id: &str) {
        self.forget_paragraph(id);
        let objects = paragraph_objects(doc, &self.containers, &self.root_path, id);
        self.paragraph_objects.insert(
            id.to_owned(),
            objects.iter().map(|(obj, _)| obj.clone()).collect(),
        );
        self.objects.extend(objects);
    }

    fn forget_paragraph(&mut self, id: &str) {
        for obj in self.paragraph_objects.remove(id).unwrap_or_default() {
            self.objects.remove(&obj);
        }
    }
}

impl Resource for RichText {
    const KIND: &'static str = "tilo.rich-text";
    const LABEL: &'static str = "Document";
    const ACTION_TYPE: &'static str = RICH_TEXT_ACTION_TYPE;

    type State = RichTextState;
    type Action = RichTextAction;

    fn least_permission() -> tilo_resource::PermissionRequirement {
        tilo_resource::PermissionRequirement::Comment
    }

    /// A comment or a reply needs only the right to comment.
    fn permission(action: &RichTextAction) -> tilo_resource::PermissionRequirement {
        match action {
            RichTextAction::AddComment { .. } | RichTextAction::ReplyToComment { .. } => {
                tilo_resource::PermissionRequirement::Comment
            }
            _ => tilo_resource::PermissionRequirement::Write,
        }
    }

    fn read(doc: &Doc) -> Result<RichTextState, ModelError> {
        Self::read_under(doc, &automerge::ROOT)
    }

    const READABLE: bool = true;

    /// The body as Markdown with the ids the block actions take; with an
    /// `item` naming a block, that block's line.
    fn read_content(
        _state: &RichTextState,
        doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut content = block_actions::read_under(doc, &automerge::ROOT, item)?;
        if let Some(map) = content.as_object_mut() {
            map.insert("kind".into(), serde_json::json!(Self::KIND));
        }
        Ok(content)
    }

    /// The ids the block actions touched.
    fn answer(doc: &Doc, action: &str, changes: &[automerge::Patch]) -> Option<serde_json::Value> {
        block_actions::action_result_under(doc, &automerge::ROOT, action, changes)
    }

    /// A change to a paragraph on screen is placed from the index kept of
    /// its objects.
    fn locate(state: &RichTextState, obj: &ObjId) -> Option<tilo_resource::Path> {
        state.objects.get(obj).cloned()
    }

    /// Text written leaves its writer's caret after it; a paragraph styled
    /// or marked leaves them on it.
    fn lands(state: &RichTextState, action: &RichTextAction) -> Option<tilo_resource::Place> {
        use tilo_resource::Place;
        let caret = |item: &str, at: usize, text: &str| Place::Caret {
            item: item.to_owned(),
            at: at.saturating_add(text.chars().count()),
        };
        match action {
            RichTextAction::AppendParagraph { text } => {
                let body = state.body();
                Some(caret(body.id_at(body.len().checked_sub(1)?)?, 0, text))
            }
            RichTextAction::InsertParagraph { after, text } => {
                let body = state.body();
                let index = body.all().position(|id| id == after)?;
                Some(caret(body.id_at(index.saturating_add(1))?, 0, text))
            }
            RichTextAction::SpliceText { block, at, ins, .. } => Some(caret(block, *at, ins)),
            RichTextAction::ReplaceSelection {
                start_block,
                start,
                ins,
                ..
            } => Some(caret(start_block, *start, ins)),
            RichTextAction::SetParagraphStyle { blocks, .. }
            | RichTextAction::ApplyNamedStyle { blocks, .. }
            | RichTextAction::SetBlockAttr { blocks, .. }
            | RichTextAction::ClearBlockAttr { blocks, .. } => {
                blocks.first().map(|block| Place::Item(block.clone()))
            }
            _ => None,
        }
    }

    /// A new document: the shared schema and one empty paragraph to type in.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        Self::initialize_under(doc, &automerge::ROOT)
    }

    fn refresh(state: &mut RichTextState, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        Self::refresh_under(state, doc, delta)
    }

    fn load(state: &mut RichTextState, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        // Every paragraph demanded and every one already read: a view that
        // shows the whole document, after an edit refreshed what it touched.
        let everything = demand.keys.is_empty()
            && demand
                .ranges
                .iter()
                .any(|range| range.start == 0 && range.end >= state.body.len());
        if everything && state.loaded.len() == state.body.len() {
            return Ok(());
        }
        let mut wanted: HashSet<String> = demand
            .positions(state.body.len())
            .filter_map(|position| state.body.id_at(position).map(str::to_owned))
            .collect();
        wanted.extend(
            demand
                .keys
                .iter()
                .filter(|id| state.body.contains(id))
                .cloned(),
        );
        let evicted: Vec<String> = state
            .loaded
            .keys()
            .filter(|id| !wanted.contains(*id))
            .cloned()
            .collect();
        for id in &evicted {
            state.loaded.remove(id);
            state.forget_paragraph(id);
        }
        let missing: Vec<String> = wanted
            .into_iter()
            .filter(|id| !state.loaded.contains_key(id))
            .collect();
        if !evicted.is_empty() || !missing.is_empty() {
            state.log.order();
        }
        if !missing.is_empty() {
            for view in model::materialize_blocks(doc, &state.containers, &missing) {
                let id = view.id.clone();
                state.loaded.insert(id.clone(), Rc::new(view));
                state.index_paragraph(doc, &id);
            }
        }
        Ok(())
    }

    /// Only typing and deleting text continue into the next keystroke; a
    /// split, a move or a format is a step of its own.
    fn continuable(action: &RichTextAction) -> bool {
        matches!(action, RichTextAction::SpliceText { .. })
    }

    /// Typing a word is one undo step: an insertion right after the previous
    /// one in the same paragraph continues it, until a space or a line of
    /// punctuation ends the word. Deleting runs the same way.
    fn coalesces(previous: &RichTextAction, next: &RichTextAction) -> bool {
        let (
            RichTextAction::SpliceText {
                region: before_region,
                block: before_block,
                at: before_at,
                del: before_del,
                ins: before_ins,
            },
            RichTextAction::SpliceText {
                region,
                block,
                at,
                del,
                ins,
            },
        ) = (previous, next)
        else {
            return false;
        };
        if before_region != region || before_block != block {
            return false;
        }
        match (*before_del, *del, before_ins.is_empty(), ins.is_empty()) {
            // Typing on: this insertion starts where the last one ended, and
            // the last one did not end the word.
            (0, 0, false, false) => {
                *at == before_at.saturating_add(before_ins.chars().count())
                    && !before_ins.ends_with(|c: char| c.is_whitespace())
            }
            // Deleting on, backwards (Backspace) or forwards (Delete).
            (1.., 1.., true, true) => at.saturating_add(*del) == *before_at || at == before_at,
            _ => false,
        }
    }

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<RichTextAction>(RICH_TEXT_ACTION_TYPE);
        // Old history still carries the word-processor type id.
        registry.register::<RichTextAction>(crate::action::RICH_TEXT_ACTION_TYPE_LEGACY);
    }

    /// Every edit, derived from [`RichTextAction`] itself, so agents read the
    /// vocabulary from the `apply` descriptor.
    fn action_schema() -> Option<serde_json::Value> {
        Some(block_actions::without_declared_variants(
            &<RichTextAction as tilo_resource::JsonSchema>::json_schema(),
        ))
    }

    /// Small named catalogue for people and agents. Positional keystrokes stay
    /// on [`APPLY`](tilo_resource::APPLY) / the action enum — not every variant.
    fn declarations() -> Vec<tilo_resource::ActionDecl<Self>> {
        use tilo_actions::{ActionParam, ParamType};
        use tilo_resource::ActionDecl;
        vec![
            ActionDecl::new(
                "set-mark",
                "Set mark",
                vec![
                    ActionParam::required_owned("name", ParamType::Text, "Mark name"),
                    ActionParam::required_owned("block", ParamType::Text, "Block id"),
                    ActionParam::required_owned("start", ParamType::Integer, "Start offset"),
                    ActionParam::required_owned("end", ParamType::Integer, "End offset"),
                    ActionParam::optional_owned(
                        "value",
                        ParamType::Text,
                        "Mark value (omit to clear)",
                    ),
                ],
                |args, _| {
                    let name = args.text("name").ok_or("missing mark name")?.to_owned();
                    let block = args.text("block").ok_or("missing block")?.to_owned();
                    let start = usize::try_from(args.integer("start").ok_or("missing start")?)
                        .unwrap_or_default();
                    let end = usize::try_from(args.integer("end").ok_or("missing end")?)
                        .unwrap_or_default();
                    let value = args.text("value").map(|text| match text {
                        "true" => action::MarkVal::Bool(true),
                        "false" => action::MarkVal::Bool(false),
                        other => action::MarkVal::Str(other.to_owned()),
                    });
                    Ok(RichTextAction::SetMark {
                        region: action::RegionKind::Body,
                        spans: vec![action::Span { block, start, end }],
                        name,
                        value,
                        also_clear: Vec::new(),
                    })
                },
            )
            .describe("Apply or clear an inline mark over a span."),
            ActionDecl::new(
                "set-style",
                "Set paragraph style",
                vec![
                    ActionParam::required_owned("block", ParamType::Text, "Block id"),
                    ActionParam::required_owned("kind", ParamType::Text, "paragraph, heading, …"),
                    ActionParam::optional_owned("level", ParamType::Integer, "Heading level"),
                ],
                |args, _| {
                    let block = args.text("block").ok_or("missing block")?.to_owned();
                    let kind = args.text("kind").ok_or("missing kind")?.to_owned();
                    let level = args.integer("level");
                    Ok(RichTextAction::SetParagraphStyle {
                        region: action::RegionKind::Body,
                        blocks: vec![block],
                        kind,
                        level,
                        ordered: None,
                    })
                },
            )
            .describe("Change a paragraph's structural kind."),
            ActionDecl::new(
                "append-paragraph",
                "Append paragraph",
                vec![ActionParam::required_owned(
                    "text",
                    ParamType::Text,
                    "The paragraph's text",
                )],
                |args, _| {
                    let text = args.text("text").ok_or("missing text")?.to_owned();
                    Ok(RichTextAction::AppendParagraph { text })
                },
            )
            // An editor with a caret refines it (`bind_declaration`) to also
            // leave its writer's caret after the text.
            .describe("Add a new paragraph containing the text at the end of the document."),
        ]
        .into_iter()
        .chain(block_actions::declarations())
        .collect()
    }

    fn views(_state: &RichTextState) -> Vec<tilo_actions::ResourceViewSpec> {
        vec![
            tilo_actions::ResourceViewSpec::new("editor", "Editor", "editor")
                .with_description("Full editing surface")
                .with_primary(true),
            tilo_actions::ResourceViewSpec::new("compact", "Compact editor", "compact")
                .with_description("Compact editing surface for nested fields"),
        ]
    }

    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        [("editor", "Editor"), ("compact", "Compact editor")]
            .into_iter()
            .filter_map(|(id, label)| {
                tilo_actions::ActionSpec::operation(
                    id,
                    label,
                    tilo_actions::OperationKind::View,
                    "tilo.rich-text.view",
                    &id,
                )
                .ok()
            })
            .collect()
    }

    /// The outline and what is on screen. A search over every paragraph of a
    /// long document reads the document; the editor offers that as its own
    /// Find.
    fn search(state: &RichTextState) -> Vec<SearchEntry> {
        let headings = state.headings().into_iter().map(|heading| SearchEntry {
            anchor: Some(heading.id),
            title: heading.text.clone(),
            text: heading.text,
            ..Default::default()
        });
        let mut on_screen: Vec<(usize, &Rc<BlockView>)> = state
            .loaded
            .values()
            .filter(|view| view.kind != "heading" && !view.text.is_empty())
            .filter_map(|view| Some((state.body.position(&view.id)?, view)))
            .collect();
        on_screen.sort_by_key(|(position, _)| *position);
        headings
            .chain(on_screen.into_iter().map(|(_, view)| SearchEntry {
                anchor: Some(view.id.clone()),
                title: view.text.chars().take(60).collect(),
                text: view.text.clone(),
                ..Default::default()
            }))
            .collect()
    }
}

impl RichText {
    /// [`Resource::read`] of rich text stored under `root`: the document's
    /// root, or the map of a rich-text field inside another resource.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `root` holds no rich text.
    pub fn read_under(doc: &Doc, root: &ObjId) -> Result<RichTextState, ModelError> {
        let c = containers_of(doc, root)?;
        let spine = model::body_spine(doc, &c);
        let featured_ids: Vec<String> = spine
            .iter()
            .filter(|entry| RichTextState::is_featured(&entry.kind))
            .map(|entry| entry.id.clone())
            .collect();
        let featured = model::materialize_blocks(doc, &c, &featured_ids)
            .into_iter()
            .map(|view| (view.id.clone(), Rc::new(view)))
            .collect();
        Ok(RichTextState {
            body: Rc::new(BodyOrder::from_spine(spine)),
            featured: Rc::new(featured),
            loaded: HashMap::new(),
            objects: HashMap::new(),
            paragraph_objects: HashMap::new(),
            header: Rc::new(model::materialize_region(doc, &c, Region::Header)),
            footer: Rc::new(model::materialize_region(doc, &c, Region::Footer)),
            notes: Rc::new(model::materialize_notes(doc, &c)),
            comments: Rc::new(model::materialize_comments(doc, &c)),
            changes: Rc::new(model::materialize_changes(doc, &c)),
            styles: Rc::new(model::all_styles(doc, &c)),
            content: Rc::new(model::referenced_content(doc, &c)),
            file_digest: file_digest(doc, &c),
            root_path: path_to(doc, &c.root),
            containers: c,
            log: ChangeLog::fresh(),
        })
    }

    /// Start rich text under `root`: the shared schema and one empty
    /// paragraph to type in. The paragraph's id depends on nothing, so the
    /// genesis is the same on every device.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn initialize_under(doc: &mut Doc, root: &ObjId) -> Result<(), ModelError> {
        let c = model::init(doc, root);
        if model::blocks_in_order(doc, &c).is_empty() {
            let mut ids = model::IdGen::new(GENESIS_IDS.to_owned());
            let mut dm = model::DocMut {
                doc,
                c: &c,
                ids: &mut ids,
            };
            dm.insert_block(None, "paragraph");
        }
        Ok(())
    }

    /// The body's text under `root`, a line per paragraph: what search
    /// indexes and a plain-text export writes.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `root` holds no rich text.
    pub fn plain_text_under(doc: &Doc, root: &ObjId) -> Result<String, ModelError> {
        let c = containers_of(doc, root)?;
        Ok(model::materialize_region(doc, &c, Region::Body)
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Read again the parts beside the body that `root` touched: the
    /// running regions, notes, review, styles, content and meta.
    fn refresh_parts(
        state: &mut RichTextState,
        doc: &Doc,
        c: &Containers,
        root: &tilo_resource::Touched,
    ) {
        if root.key(KEY_HEADER) != Touch::Untouched {
            state.header = Rc::new(model::materialize_region(doc, c, Region::Header));
        }
        if root.key(KEY_FOOTER) != Touch::Untouched {
            state.footer = Rc::new(model::materialize_region(doc, c, Region::Footer));
        }
        if root.key(KEY_NOTES) != Touch::Untouched {
            state.notes = Rc::new(model::materialize_notes(doc, c));
        }
        if root.key(KEY_COMMENTS) != Touch::Untouched {
            state.comments = Rc::new(model::materialize_comments(doc, c));
        }
        if root.key(KEY_CHANGES) != Touch::Untouched {
            state.changes = Rc::new(model::materialize_changes(doc, c));
        }
        if root.key(KEY_STYLES) != Touch::Untouched {
            state.styles = Rc::new(model::all_styles(doc, c));
        }
        if root.key(KEY_BLOBS) != Touch::Untouched {
            state.content = Rc::new(model::referenced_content(doc, c));
        }
        if root.key(KEY_META) != Touch::Untouched {
            state.file_digest = file_digest(doc, c);
        }
    }

    /// [`Resource::refresh`] for rich text under any root: the state knows
    /// where it was read from.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the rich text is no longer there.
    pub fn refresh_under(
        state: &mut RichTextState,
        doc: &Doc,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if delta.is_empty() {
            return Ok(());
        }
        let root = delta.under(&state.containers.root);
        if delta.is_everything() || root.is_all() || root.key(KEY_BLOCKS) == Touch::Written {
            *state = Self::read_under(doc, &state.containers.root)?;
            return Ok(());
        }
        // A lazily created container (the first header, note or comment)
        // is a new object: find it before reading it.
        if root.written().next().is_some() {
            state.containers = containers_of(doc, &state.containers.root)?;
        }
        let c = state.containers.clone();
        Self::refresh_parts(state, doc, &c, &root);
        if root.key(KEY_BLOCKS) == Touch::Untouched {
            return Ok(());
        }
        let touched = delta.under(&c.blocks);
        for id in touched.keys() {
            let entry = model::body_spine_entry(doc, &c, id);
            // The paragraph's place: new, gone, or moved.
            let placed = state.body.key(id).map(<[u8]>::to_vec);
            match &entry {
                Some(entry) if placed.as_deref() != Some(entry.order.as_slice()) => {
                    Rc::make_mut(&mut state.body).insert(id, &entry.order);
                    state.log.order();
                }
                None if placed.is_some() => {
                    Rc::make_mut(&mut state.body).remove(id);
                    state.log.order();
                }
                _ => {}
            }
            // Its content, when on screen or in the outline. A paragraph
            // whose order key alone changed keeps its view.
            let order_only = touched.key(id) == Touch::Below
                && block_obj(doc, &c.blocks, id).is_some_and(|obj| {
                    let inner = delta.under(&obj);
                    inner.keys().all(|key| key == BLOCK_ORDER) && !inner.is_all()
                });
            let was_featured = state.featured.contains_key(id);
            let is_featured = entry
                .as_ref()
                .is_some_and(|entry| RichTextState::is_featured(&entry.kind));
            let on_screen = state.loaded.contains_key(id);
            if entry.is_none() {
                if state.loaded.contains_key(id) {
                    state.log.order();
                }
                state.loaded.remove(id);
                state.forget_paragraph(id);
                if was_featured {
                    Rc::make_mut(&mut state.featured).remove(id);
                }
                continue;
            }
            if order_only || !(on_screen || was_featured || is_featured) {
                continue;
            }
            let Some(view) = model::materialize_blocks(doc, &c, &[id.to_owned()]).pop() else {
                continue;
            };
            // A paragraph whose content came out the same keeps its shared
            // value, so views can tell a changed paragraph by pointer.
            let view = match state.loaded.get(id).or_else(|| state.featured.get(id)) {
                Some(old) if **old == view => Rc::clone(old),
                _ => {
                    if on_screen {
                        state.log.content(id);
                    }
                    Rc::new(view)
                }
            };
            if RichTextState::is_featured(&view.kind) {
                Rc::make_mut(&mut state.featured).insert(id.to_owned(), Rc::clone(&view));
            } else if was_featured {
                Rc::make_mut(&mut state.featured).remove(id);
            }
            if on_screen {
                state.loaded.insert(id.to_owned(), view);
                state.index_paragraph(doc, id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod schema_tests;

#[cfg(test)]
mod block_tests;
