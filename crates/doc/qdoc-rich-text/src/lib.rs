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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use automerge::{ObjId, ObjType, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_resource::{Delta, Demand, Doc, ModelError, Resource, SearchEntry, Touch};

use qdoc::model::{self, Containers, Region};
use qdoc::schema::{
    BLOCK_ATTRS, BLOCK_ORDER, BLOCK_TEXT, KEY_BLOBS, KEY_BLOCKS, KEY_CHANGES, KEY_COMMENTS,
    KEY_FOOTER, KEY_HEADER, KEY_NOTES,
};
use qdoc::snapshot::{AttrVal, BlockView, ChangeView, CommentView, NoteView};

pub use action::{RICH_TEXT_ACTION_TYPE, RichTextAction};

/// The styles every document has; they cannot be deleted.
pub const BUILTIN_STYLES: [&str; 6] = ["body", "heading1", "heading2", "heading3", "list", "quote"];

/// One block of a copied rich fragment — text plus its formatting (marks and
/// paragraph attributes), with mark offsets relative to the block's own text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct ClipBlock {
    pub kind: String,
    pub text: String,
    pub marks: Vec<qdoc::document::MarkSpan>,
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
    /// Every content address the document refers to (its images and
    /// preserved package parts); the bytes live in the caller's store.
    content: Rc<BTreeSet<String>>,
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
            && self.content == other.content
    }
}

impl RichTextState {
    /// Where the document's containers live.
    pub fn containers(&self) -> &Containers {
        &self.containers
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

    /// Up to `limit` blocks of `kind` nearest position `around`, in
    /// document order: what a view near there lists (the sections, images
    /// and tables around a caret) at the cost of the neighbourhood it looks
    /// through, not of the document.
    pub fn featured_near(&self, kind: &str, around: usize, limit: usize) -> Vec<Rc<BlockView>> {
        /// How far either way to look.
        const REACH: usize = 20_000;
        // Few enough to list them all.
        if self
            .featured
            .values()
            .filter(|view| view.kind == kind)
            .count()
            <= limit
        {
            return self.featured(kind);
        }
        let mut found: Vec<(usize, Rc<BlockView>)> = Vec::new();
        let len = self.body.len();
        let at = |position: usize| {
            self.body
                .id_at(position)
                .and_then(|id| self.featured.get(id))
                .filter(|view| view.kind == kind)
                .map(|view| (position, Rc::clone(view)))
        };
        for step in 0..=REACH {
            let before = around.checked_sub(step);
            let after = around.saturating_add(step);
            if before.is_none() && after >= len {
                break;
            }
            found.extend(before.and_then(at));
            if step > 0 && after < len {
                found.extend(at(after));
            }
            if found.len() >= limit {
                break;
            }
        }
        found.sort_by_key(|(position, _)| *position);
        found.truncate(limit);
        found.into_iter().map(|(_, view)| view).collect()
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
    const KIND: &'static str = "qdoc.rich-text";
    const LABEL: &'static str = "Document";
    const ACTION_TYPE: &'static str = RICH_TEXT_ACTION_TYPE;

    type State = RichTextState;
    type Action = RichTextAction;

    fn read(doc: &Doc) -> Result<RichTextState, ModelError> {
        Self::read_under(doc, &automerge::ROOT)
    }

    /// A change to a paragraph on screen is placed from the index kept of
    /// its objects.
    fn locate(state: &RichTextState, obj: &ObjId) -> Option<tilo_resource::Path> {
        state.objects.get(obj).cloned()
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
        if !missing.is_empty() {
            for view in model::materialize_blocks(doc, &state.containers, &missing) {
                let id = view.id.clone();
                state.loaded.insert(id.clone(), Rc::new(view));
                state.index_paragraph(doc, &id);
            }
        }
        Ok(())
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

    /// Recording who wrote text is part of the edit that wrote it.
    fn annotates(action: &RichTextAction) -> bool {
        matches!(action, RichTextAction::SetMark { name, .. } if name == qdoc::schema::MARK_AUTHOR)
    }

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<RichTextAction>(RICH_TEXT_ACTION_TYPE);
    }

    /// Every edit, derived from [`RichTextAction`] itself, so agents read the
    /// vocabulary from the `apply` descriptor.
    fn action_schema() -> Option<serde_json::Value> {
        Some(<RichTextAction as tilo_resource::JsonSchema>::json_schema())
    }

    /// The outline and what is on screen. A search over every paragraph of a
    /// long document reads the document; the editor offers that as its own
    /// Find.
    fn search(state: &RichTextState) -> Vec<SearchEntry> {
        let headings = state.headings().into_iter().map(|heading| SearchEntry {
            anchor: Some(heading.id),
            title: heading.text.clone(),
            text: heading.text,
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
            content: Rc::new(model::referenced_content(doc, &c)),
            root_path: path_to(doc, &c.root),
            containers: c,
        })
    }

    /// Start rich text under `root`: the shared schema and one empty
    /// paragraph to type in.
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn initialize_under(doc: &mut Doc, root: &ObjId) -> Result<(), ModelError> {
        let c = model::init(doc, root);
        if model::blocks_in_order(doc, &c).is_empty() {
            let mut ids = model::IdGen::new(doc.get_actor().to_hex_string());
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
        if root.key(KEY_HEADER) != Touch::Untouched {
            state.header = Rc::new(model::materialize_region(doc, &c, Region::Header));
        }
        if root.key(KEY_FOOTER) != Touch::Untouched {
            state.footer = Rc::new(model::materialize_region(doc, &c, Region::Footer));
        }
        if root.key(KEY_NOTES) != Touch::Untouched {
            state.notes = Rc::new(model::materialize_notes(doc, &c));
        }
        if root.key(KEY_COMMENTS) != Touch::Untouched {
            state.comments = Rc::new(model::materialize_comments(doc, &c));
        }
        if root.key(KEY_CHANGES) != Touch::Untouched {
            state.changes = Rc::new(model::materialize_changes(doc, &c));
        }
        if root.key(KEY_BLOBS) != Touch::Untouched {
            state.content = Rc::new(model::referenced_content(doc, &c));
        }
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
                }
                None if placed.is_some() => Rc::make_mut(&mut state.body).remove(id),
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
                _ => Rc::new(view),
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
