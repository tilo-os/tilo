//! The word processor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation the editor performs on its shared `qdoc`
//! rich-text document — a text splice, a formatting mark, a paragraph attribute,
//! a structural insert/delete — is a [`WordProcessorAction`]: a small
//! serializable value that deterministically mutates the CRDT. Because the
//! document already lives in the shared `AutoCommit` (the canonical `qdoc`
//! schema under `ROOT` — see [`crate::EditorDoc`]), an action replicates and
//! replays for free, and the exact same [`EncodedAction`] is what the on-screen
//! affordances (the formatting toolbar, the paragraph panel, the structure
//! menus) dispatch and what the Ctrl+K palette dispatches — one source of truth,
//! one replay path.
//!
//! Presentation — the caret, the selection, scroll/zoom, which tool or panel is
//! open, the live per-keystroke edit buffer before it is committed — stays out
//! of the document and never becomes an action.
//!
//! ## Determinism contract
//!
//! [`Action::apply`] is pure with respect to its payload: it never reads the
//! wall-clock or the RNG. Every offset, id and value the mutation needs is
//! carried in the payload, minted at *authoring* time. Newly-created block ids
//! (a table's cells, an inserted image, a page break's fresh paragraph) are
//! minted **inside** `apply` from a [`seeded_id_gen`](qdoc::model::seeded_id_gen)
//! — deterministic because the executor pins the Automerge actor from the
//! action's id, so `{actor}.{counter}` reproduces byte-identically on every
//! replica. Sets of blocks/spans are sorted before iterating, so a replay emits
//! the identical op sequence. A malformed payload (an unknown block id) is a
//! **no-op**, not an error, so a bad replay can't wedge a peer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};
use serde::{Deserialize, Serialize};

use qdoc::model::Region;
use qdoc::{Containers, DocMut};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

/// Registry `type_id` for every word-processor action.
pub const WORD_PROCESSOR_ACTION_TYPE: &str = "word-processor.action";

/// Which page region a text/formatting op addresses. Mirrors [`qdoc::model::Region`]
/// (which is not itself serializable) so the region travels in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    Body,
    Header,
    Footer,
}

impl RegionKind {
    pub fn to_region(self) -> Region {
        match self {
            RegionKind::Body => Region::Body,
            RegionKind::Header => Region::Header,
            RegionKind::Footer => Region::Footer,
        }
    }

    pub fn from_region(r: Region) -> Self {
        match r {
            Region::Body => RegionKind::Body,
            Region::Header => RegionKind::Header,
            Region::Footer => RegionKind::Footer,
        }
    }
}

/// A serializable inline-mark value (the character-format payload). `Bool` is a
/// toggle mark (bold/italic/underline/strike/super/sub); `Int` is a size; `Str`
/// is a font family / colour / highlight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkVal {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// A serializable block-attribute value. Maps onto the scalar an attr stores
/// (`align`/`style`/`tab_stops` are `Str`, `level`/image dims are `Int`,
/// `indent`/`line_spacing` are `Float`, `page_break_before`/flips are `Bool`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttrVal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// One `(block, [start, end))` range a formatting mark covers. A selection that
/// spans several blocks is carried as one span per touched block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub block: String,
    pub start: usize,
    pub end: usize,
}

/// Every collaborative, document-mutating operation the word processor exports.
///
/// The set is deliberately positional: each variant names the exact block ids
/// and character offsets it touches, so [`Action::apply`] is a pure write and a
/// replay reproduces the identical change. Live per-keystroke typing is *not*
/// here — it splices directly into the per-block CRDT `Text` (the ephemeral
/// live-edit path); a discrete text edit (Find & Replace, a scripted splice) is
/// [`Self::SpliceText`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WordProcessorAction {
    /// Replace `[at, at+del)` of `block` with `ins` (insert = `del` 0; delete =
    /// `ins` empty). The deterministic text primitive.
    SpliceText {
        region: RegionKind,
        block: String,
        at: usize,
        del: usize,
        ins: String,
    },
    /// Apply or clear an inline mark over `spans`. `value` `None` removes the
    /// mark; `Some` replaces it (any prior value of the same mark over each span
    /// is removed first, so a re-colour/re-size overwrites rather than stacks).
    /// `also_clear` names sibling marks to drop over the same spans first (so a
    /// super/subscript pair stays mutually exclusive).
    SetMark {
        region: RegionKind,
        spans: Vec<Span>,
        name: String,
        value: Option<MarkVal>,
        #[serde(default)]
        also_clear: Vec<String>,
    },
    /// Set a paragraph's structural kind (`heading`/`paragraph`/`list-item`/…)
    /// on each of `blocks`, plus its `level` / `ordered` attrs when given.
    SetParagraphStyle {
        blocks: Vec<String>,
        kind: String,
        level: Option<i64>,
        ordered: Option<bool>,
    },
    /// Apply a named paragraph style to each of `blocks`: set the `style` attr,
    /// and — when the style implies a structural kind (a built-in) — the block
    /// `kind` / `level` / `ordered` too. One action so a style pick is a single
    /// replayable operation / one undo boundary (rather than a kind write and a
    /// style write racing as two changes).
    ApplyNamedStyle {
        blocks: Vec<String>,
        style: String,
        kind: Option<String>,
        level: Option<i64>,
        ordered: Option<bool>,
    },
    /// Set one block attribute (`align`, `page_break_before`, `indent`,
    /// `first_line_indent`, `line_spacing`, `tab_stops`, `style`, image
    /// `width`/`height`/`flip_*`) to `value` on each of `blocks`.
    SetBlockAttr {
        region: RegionKind,
        blocks: Vec<String>,
        key: String,
        value: AttrVal,
    },
    /// Insert a `rows`×`cols` table after `after`. Cells are consecutive `cell`
    /// blocks sharing a freshly-minted `table` id, tagged with `row`/`col`.
    InsertTable {
        after: String,
        rows: usize,
        cols: usize,
    },
    /// Insert an image (blob `bytes`, optional intrinsic `width`/`height`) at
    /// `(block, at)`: the caret block is split so the tail follows the image in a
    /// fresh paragraph beneath it.
    InsertImage {
        block: String,
        at: usize,
        bytes: Vec<u8>,
        width: Option<i64>,
        height: Option<i64>,
    },
    /// Insert a manual page break at `(block, at)`: split the block like Enter and
    /// mark the new block to start on a fresh page.
    InsertPageBreak {
        block: String,
        at: usize,
    },
    /// Remove `blocks` (an image, or every cell of a table). Ids are sorted
    /// before removal so replicas emit the identical op sequence.
    RemoveBlocks {
        blocks: Vec<String>,
    },
    /// Replace the whole document body with `markdown` (re-import a template /
    /// restore a version): every existing body block is removed, then the
    /// Markdown is imported as fresh blocks.
    ReplaceBody {
        markdown: String,
    },
}

// ---------------------------------------------------------------------------
// Region-aware mark helpers (mirroring `WordProcessor::region_*`), so `apply`
// writes marks the same way the live editor does across body / header / footer.
// ---------------------------------------------------------------------------

fn region_map_obj(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(root, key) {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

fn region_block_obj(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(container, id) {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

/// Resolve a region's block-container map object live from the document root.
fn region_container_obj(doc: &AutoCommit, c: &Containers, region: Region) -> Option<ObjId> {
    match region {
        Region::Body => Some(c.blocks.clone()),
        Region::Header => region_map_obj(doc, &c.root, qdoc::schema::KEY_HEADER),
        Region::Footer => region_map_obj(doc, &c.root, qdoc::schema::KEY_FOOTER),
    }
}

fn add_mark_region(dm: &mut DocMut, region: Region, span: &Span, name: &str, value: &MarkVal) {
    match region {
        Region::Body => match value {
            MarkVal::Bool(v) => dm.add_mark(&span.block, span.start, span.end, name, *v),
            MarkVal::Int(n) => dm.add_mark(&span.block, span.start, span.end, name, *n),
            MarkVal::Str(s) => dm.add_mark(&span.block, span.start, span.end, name, s.as_str()),
        },
        r => match value {
            MarkVal::Bool(v) => dm.add_mark_in(r, &span.block, span.start, span.end, name, *v),
            MarkVal::Int(n) => dm.add_mark_in(r, &span.block, span.start, span.end, name, *n),
            MarkVal::Str(s) => dm.add_mark_in(r, &span.block, span.start, span.end, name, s.as_str()),
        },
    }
}

fn remove_mark_region(dm: &mut DocMut, c: &Containers, region: Region, span: &Span, name: &str) {
    if region == Region::Body {
        dm.remove_mark(&span.block, span.start, span.end, name);
        return;
    }
    // Header/Footer: no `remove_mark_in` op — clear the mark directly on the
    // region block's Text object (mirrors the body `remove_mark`'s
    // `unmark(.., ExpandMark::After)`).
    if let Some(container) = region_container_obj(dm.doc, c, region) {
        if let Some(obj) = region_block_obj(dm.doc, &container, &span.block) {
            if let Ok(Some((automerge::Value::Object(automerge::ObjType::Text), tid))) =
                dm.doc.get(&obj, qdoc::schema::BLOCK_TEXT)
            {
                let _ = dm.doc.unmark(
                    &tid,
                    name,
                    span.start,
                    span.end,
                    automerge::marks::ExpandMark::After,
                );
            }
        }
    }
}

fn set_attr_region(dm: &mut DocMut, region: Region, block: &str, key: &str, value: &AttrVal) {
    match region {
        Region::Body => match value {
            AttrVal::Bool(v) => dm.set_attr(block, key, *v),
            AttrVal::Int(n) => dm.set_attr(block, key, *n),
            AttrVal::Float(f) => dm.set_attr(block, key, *f),
            AttrVal::Str(s) => dm.set_attr(block, key, s.as_str()),
        },
        r => match value {
            AttrVal::Bool(v) => dm.set_attr_in(r, block, key, *v),
            AttrVal::Int(n) => dm.set_attr_in(r, block, key, *n),
            AttrVal::Float(f) => dm.set_attr_in(r, block, key, *f),
            AttrVal::Str(s) => dm.set_attr_in(r, block, key, s.as_str()),
        },
    }
}

impl Action for WordProcessorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Bind to the canonical schema under ROOT. An unbound document (no
        // schema yet) is a no-op — nothing to mutate, and a bad replay can't
        // wedge.
        let Ok(c) = qdoc::model::open(doc, &automerge::ROOT) else {
            return Ok(());
        };
        // A generator seeded past every existing id, under the executor's pinned
        // actor — so any freshly-minted block id is `{action-actor}.{n}`,
        // byte-identical on origin and replay.
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);

        match self {
            WordProcessorAction::SpliceText {
                region,
                block,
                at,
                del,
                ins,
            } => {
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                dm.splice_text_in(region.to_region(), block, *at, *del as isize, ins);
            }

            WordProcessorAction::SetMark {
                region,
                spans,
                name,
                value,
                also_clear,
            } => {
                let r = region.to_region();
                let mut sorted = spans.clone();
                sorted.sort_by(|a, b| {
                    (a.block.as_str(), a.start, a.end).cmp(&(b.block.as_str(), b.start, b.end))
                });
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for span in &sorted {
                    if span.start >= span.end {
                        continue;
                    }
                    for sib in also_clear {
                        remove_mark_region(&mut dm, &c, r, span, sib);
                    }
                    match value {
                        None => remove_mark_region(&mut dm, &c, r, span, name),
                        Some(v) => {
                            // Replace: drop any prior value of the same mark
                            // before re-adding so values overwrite, not stack.
                            remove_mark_region(&mut dm, &c, r, span, name);
                            add_mark_region(&mut dm, r, span, name, v);
                        }
                    }
                }
            }

            WordProcessorAction::SetParagraphStyle {
                blocks,
                kind,
                level,
                ordered,
            } => {
                let mut sorted = blocks.clone();
                sorted.sort();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for block in &sorted {
                    dm.set_kind(block, kind);
                    if let Some(l) = level {
                        dm.set_attr(block, "level", *l);
                    }
                    if let Some(o) = ordered {
                        dm.set_attr(block, "ordered", *o);
                    }
                }
            }

            WordProcessorAction::ApplyNamedStyle {
                blocks,
                style,
                kind,
                level,
                ordered,
            } => {
                let mut sorted = blocks.clone();
                sorted.sort();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for block in &sorted {
                    if let Some(k) = kind {
                        dm.set_kind(block, k);
                        if let Some(l) = level {
                            dm.set_attr(block, "level", *l);
                        }
                        if let Some(o) = ordered {
                            dm.set_attr(block, "ordered", *o);
                        }
                    }
                    dm.set_attr(block, "style", style.as_str());
                }
            }

            WordProcessorAction::SetBlockAttr {
                region,
                blocks,
                key,
                value,
            } => {
                let r = region.to_region();
                let mut sorted = blocks.clone();
                sorted.sort();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for block in &sorted {
                    set_attr_region(&mut dm, r, block, key, value);
                }
            }

            WordProcessorAction::InsertTable { after, rows, cols } => {
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                let table_id = dm.ids.mint(); // one id shared by every cell
                let mut prev = Some(after.clone());
                for row in 0..*rows {
                    for col in 0..*cols {
                        let id = dm.insert_block(prev.as_deref(), "cell");
                        dm.set_attr(&id, "table", table_id.as_str());
                        dm.set_attr(&id, "row", row as i64);
                        dm.set_attr(&id, "col", col as i64);
                        prev = Some(id);
                    }
                }
            }

            WordProcessorAction::InsertImage {
                block,
                at,
                bytes,
                width,
                height,
            } => {
                let text = qdoc::model::block_text(doc, &c, block).unwrap_or_default();
                let char_count = text.chars().count();
                let tail: String = text.chars().skip(*at).collect();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                let hash = dm.put_blob(bytes);
                if *at < char_count {
                    dm.splice_text(block, *at, (char_count - *at) as isize, "");
                }
                let img = dm.insert_block(Some(block), "image");
                dm.set_attr(&img, "blob", hash.as_str());
                if let Some(w) = width {
                    dm.set_attr(&img, "width", *w);
                }
                if let Some(h) = height {
                    dm.set_attr(&img, "height", *h);
                }
                let after = dm.insert_block(Some(&img), "paragraph");
                if !tail.is_empty() {
                    dm.splice_text(&after, 0, 0, &tail);
                }
            }

            WordProcessorAction::InsertPageBreak { block, at } => {
                // Split the block at the caret (like Enter): the tail moves into a
                // fresh paragraph that starts on a new page.
                let text = qdoc::model::block_text(doc, &c, block).unwrap_or_default();
                let char_count = text.chars().count();
                let tail: String = text.chars().skip(*at).collect();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                if *at < char_count {
                    dm.splice_text(block, *at, (char_count - *at) as isize, "");
                }
                let new = dm.insert_block(Some(block), "paragraph");
                if !tail.is_empty() {
                    dm.splice_text(&new, 0, 0, &tail);
                }
                dm.set_attr(&new, "page_break_before", true);
            }

            WordProcessorAction::RemoveBlocks { blocks } => {
                let mut sorted = blocks.clone();
                sorted.sort();
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for block in &sorted {
                    dm.remove_block(block);
                }
            }

            WordProcessorAction::ReplaceBody { markdown } => {
                use qdoc::FormatProvider;
                let existing = qdoc::model::blocks_in_order(doc, &c);
                let mut dm = DocMut { doc, c: &c, ids: &mut ids };
                for id in &existing {
                    dm.remove_block(id);
                }
                // A malformed template can't wedge a replay: the body-clear stays
                // in the pinned change either way.
                let _ = qdoc::MarkdownCodec.import(markdown.as_bytes(), &mut dm);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the word processor's action decoder for palette execution + remote
/// replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<WordProcessorAction>(WORD_PROCESSOR_ACTION_TYPE);
}

/// Encode any [`WordProcessorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch. The single point that turns an action
/// into a replayable envelope.
pub fn encode(action: &WordProcessorAction) -> EncodedAction {
    EncodedAction::encode(WORD_PROCESSOR_ACTION_TYPE, action)
        .expect("WordProcessorAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::widget`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Apply (or, with `value` `None`, clear) an inline mark over `spans`.
pub fn set_mark_action(
    region: RegionKind,
    spans: Vec<Span>,
    name: &str,
    value: Option<MarkVal>,
    also_clear: Vec<String>,
) -> WordProcessorAction {
    WordProcessorAction::SetMark {
        region,
        spans,
        name: name.to_string(),
        value,
        also_clear,
    }
}

/// Set one block attribute on `blocks`.
pub fn set_block_attr_action(
    region: RegionKind,
    blocks: Vec<String>,
    key: &str,
    value: AttrVal,
) -> WordProcessorAction {
    WordProcessorAction::SetBlockAttr {
        region,
        blocks,
        key: key.to_string(),
        value,
    }
}

/// Set alignment (`left`/`center`/`right`/`justify`) on `blocks`.
pub fn set_align_action(blocks: Vec<String>, align: &str) -> WordProcessorAction {
    set_block_attr_action(RegionKind::Body, blocks, "align", AttrVal::Str(align.to_string()))
}

/// Apply a named paragraph style (and, for a built-in, its structural kind) to
/// `blocks` in one action.
pub fn apply_named_style_action(
    blocks: Vec<String>,
    style: &str,
    kind: Option<&str>,
    level: Option<i64>,
    ordered: Option<bool>,
) -> WordProcessorAction {
    WordProcessorAction::ApplyNamedStyle {
        blocks,
        style: style.to_string(),
        kind: kind.map(|k| k.to_string()),
        level,
        ordered,
    }
}

/// Set the paragraph kind/level/ordered on `blocks`.
pub fn set_paragraph_style_action(
    blocks: Vec<String>,
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) -> WordProcessorAction {
    WordProcessorAction::SetParagraphStyle {
        blocks,
        kind: kind.to_string(),
        level,
        ordered,
    }
}

/// Insert a `rows`×`cols` table after `after`.
pub fn insert_table_action(after: &str, rows: usize, cols: usize) -> WordProcessorAction {
    WordProcessorAction::InsertTable {
        after: after.to_string(),
        rows,
        cols,
    }
}

/// Insert a manual page break at `(block, at)`.
pub fn insert_page_break_action(block: &str, at: usize) -> WordProcessorAction {
    WordProcessorAction::InsertPageBreak {
        block: block.to_string(),
        at,
    }
}

/// Insert an image at `(block, at)`.
pub fn insert_image_action(
    block: &str,
    at: usize,
    bytes: Vec<u8>,
    width: Option<i64>,
    height: Option<i64>,
) -> WordProcessorAction {
    WordProcessorAction::InsertImage {
        block: block.to_string(),
        at,
        bytes,
        width,
        height,
    }
}

/// Remove `blocks` (an image, or a table's cells).
pub fn remove_blocks_action(blocks: Vec<String>) -> WordProcessorAction {
    WordProcessorAction::RemoveBlocks { blocks }
}

/// Replace the whole document body with `markdown`.
pub fn replace_body_action(markdown: &str) -> WordProcessorAction {
    WordProcessorAction::ReplaceBody {
        markdown: markdown.to_string(),
    }
}

/// Replace `[start, end)` of `block` with `text` (Find & Replace).
pub fn splice_text_action(
    region: RegionKind,
    block: &str,
    start: usize,
    end: usize,
    text: &str,
) -> WordProcessorAction {
    WordProcessorAction::SpliceText {
        region,
        block: block.to_string(),
        at: start,
        del: end.saturating_sub(start),
        ins: text.to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Ctrl+K needs to build the document's resource tree:
/// the caret block/offset the structural inserts anchor on, and the block ids
/// the block-level formatting targets (the selection, or the caret block).
/// Everything here derives from state the app owns; the resource children
/// (images / tables / sections) are enumerated from the shared document.
#[derive(Debug, Clone)]
pub struct WpResourceCtx {
    pub caret_block: String,
    pub caret_offset: usize,
    /// The blocks a block-level action targets — the selection, or the caret
    /// block when there is no selection.
    pub target_blocks: Vec<String>,
}

fn immediate_spec(id: &str, label: &str, action: &WordProcessorAction) -> ActionSpec {
    ActionSpec::immediate(id, label, WORD_PROCESSOR_ACTION_TYPE, action)
        .expect("encode word-processor action")
}

/// A short label for a structural block in the palette.
fn short(text: &str, fallback: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim();
    if t.is_empty() {
        fallback.to_string()
    } else if t.chars().count() > 48 {
        t.chars().take(45).collect::<String>() + "…"
    } else {
        t.to_string()
    }
}

/// Build the resource tree the word processor currently has open: the document
/// (exporting the structural inserts + paragraph alignment over the current
/// target blocks) and a child resource per image, table and heading, each
/// exporting the document-mutating operations the data model implies.
pub fn provide_resources(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("word-processor.document", "word_processor.document", "Document");

    // Document-level structural inserts, anchored on the caret.
    root = root.with_action(immediate_spec(
        "insert-table",
        "Insert table",
        &insert_table_action(&ctx.caret_block, 2, 2),
    ));
    root = root.with_action(immediate_spec(
        "insert-page-break",
        "Insert page break",
        &insert_page_break_action(&ctx.caret_block, ctx.caret_offset),
    ));

    // Document-level paragraph alignment over the current target blocks.
    for (id, label, align) in [
        ("align-left", "Align left", "left"),
        ("align-center", "Align centre", "center"),
        ("align-right", "Align right", "right"),
        ("align-justify", "Justify", "justify"),
    ] {
        root = root.with_action(immediate_spec(
            id,
            label,
            &set_align_action(ctx.target_blocks.clone(), align),
        ));
    }

    // Structural children enumerated from the shared document.
    let Ok(c) = qdoc::model::open(doc, &automerge::ROOT) else {
        return vec![root];
    };
    let snap = qdoc::model::materialize(doc, &c);

    // Images → a resource each, with a delete action.
    for b in snap.blocks.iter().filter(|b| b.kind == "image") {
        let child = Resource::new(
            format!("word-processor.image:{}", b.id),
            "word_processor.image",
            "Image",
        )
        .with_action(immediate_spec(
            "delete",
            "Delete image",
            &remove_blocks_action(vec![b.id.clone()]),
        ));
        root = root.with_child(child);
    }

    // Tables (grouped by the shared `table` id, in first-cell document order) →
    // a resource each, deleting every cell of the table.
    let mut table_ids: Vec<String> = Vec::new();
    for b in snap.blocks.iter().filter(|b| b.kind == "cell") {
        if let Some(qdoc::AttrVal::Str(tid)) = b.attrs.get("table") {
            if !table_ids.contains(tid) {
                table_ids.push(tid.clone());
            }
        }
    }
    for tid in &table_ids {
        let cells: Vec<String> = snap
            .blocks
            .iter()
            .filter(|b| {
                b.kind == "cell"
                    && matches!(b.attrs.get("table"), Some(qdoc::AttrVal::Str(s)) if s == tid)
            })
            .map(|b| b.id.clone())
            .collect();
        let child = Resource::new(
            format!("word-processor.table:{tid}"),
            "word_processor.table",
            "Table",
        )
        .with_action(immediate_spec(
            "delete",
            "Delete table",
            &remove_blocks_action(cells),
        ));
        root = root.with_child(child);
    }

    // Headings → a `section` resource each, labelled by the heading text, with
    // alignment actions over that heading block.
    for b in snap.blocks.iter().filter(|b| b.kind == "heading") {
        let mut child = Resource::new(
            format!("word-processor.section:{}", b.id),
            "word_processor.section",
            short(&b.text, "Heading"),
        );
        for (id, label, align) in [
            ("align-left", "Align left", "left"),
            ("align-center", "Align centre", "center"),
            ("align-right", "Align right", "right"),
        ] {
            child = child.with_action(immediate_spec(
                id,
                label,
                &set_align_action(vec![b.id.clone()], align),
            ));
        }
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::widget::WordProcessorWidget {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.wp_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// A `ui::State` holding a canonical document seeded from `markdown`, plus the
    /// ordered body block ids.
    fn seeded(markdown: &str) -> (ui::State, Vec<String>) {
        let mut state = ui::State::new();
        let mut ed = crate::EditorDoc::bind(&mut state.doc, &state.root);
        {
            use qdoc::FormatProvider;
            let mut dm = ed.edit(&mut state.doc);
            qdoc::MarkdownCodec.import(markdown.as_bytes(), &mut dm).unwrap();
        }
        let ids = qdoc::model::blocks_in_order(&state.doc, &ed.containers);
        (state, ids)
    }

    fn body_blocks(doc: &AutoCommit) -> Vec<qdoc::snapshot::BlockView> {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::materialize(doc, &c).blocks
    }

    fn text_of(doc: &AutoCommit, block: &str) -> String {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::block_text(doc, &c, block).unwrap_or_default()
    }

    /// Dispatch an action through the qOS executor with a discarding transport
    /// (the local, single-peer path the affordances take).
    fn dispatch(doc: &mut AutoCommit, action: &WordProcessorAction) {
        let encoded = encode(action);
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, action, encoded, &mut tx).unwrap();
    }

    /// A text splice through an action mutates the document, and replays
    /// byte-identically on a second, independent replica (equal head hashes).
    #[test]
    fn splice_text_mutates_and_replays_byte_identical() {
        let (mut a, ids) = seeded("Hello world\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "precondition: same base state");

        // Replace "world" with "there".
        let action = splice_text_action(RegionKind::Body, &block, 6, 11, "there");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(text_of(&a.doc, &block), "Hello there", "the splice landed");
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "replay reproduced the identical change"
        );
    }

    /// A formatting mark over a selection replays byte-identically (the marks
    /// path, not just a scalar write).
    #[test]
    fn set_mark_replays_byte_identical() {
        let (mut a, ids) = seeded("Hello world\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = set_mark_action(
            RegionKind::Body,
            vec![Span { block: block.clone(), start: 0, end: 5 }],
            "strong",
            Some(MarkVal::Bool(true)),
            vec![],
        );
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "mark replayed identically");
        let marks = body_blocks(&b.doc)
            .into_iter()
            .find(|x| x.id == block)
            .map(|x| x.marks)
            .unwrap_or_default();
        assert!(
            marks.iter().any(|m| m.name == "strong" && m.start == 0 && m.end == 5),
            "the bold run is present on the peer: {marks:?}"
        );
    }

    /// A paragraph-style change (kind + level) replays byte-identically.
    #[test]
    fn set_paragraph_style_replays_byte_identical() {
        let (mut a, ids) = seeded("Title\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = set_paragraph_style_action(vec![block.clone()], "heading", Some(1), None);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "style replayed identically");
        assert_eq!(
            body_blocks(&b.doc).into_iter().find(|x| x.id == block).unwrap().kind,
            "heading",
            "the block became a heading on the peer"
        );
    }

    /// Inserting a table through an action creates the cells (ids minted under
    /// the pinned actor) and replays byte-identically — proving deterministic
    /// id minting inside `apply`.
    #[test]
    fn insert_table_replays_byte_identical() {
        let (mut a, ids) = seeded("Intro\n");
        let after = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = insert_table_action(&after, 2, 3);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "table replayed identically");
        let cells_a = body_blocks(&a.doc).into_iter().filter(|x| x.kind == "cell").count();
        let cells_b = body_blocks(&b.doc).into_iter().filter(|x| x.kind == "cell").count();
        assert_eq!(cells_a, 6, "2x3 = six cells");
        assert_eq!(cells_a, cells_b, "the same cells on both replicas");
    }

    /// A page break splits the block and marks the tail's fresh block to start a
    /// new page, and replays byte-identically.
    #[test]
    fn insert_page_break_replays_byte_identical() {
        let (mut a, ids) = seeded("onetwo\n");
        let block = ids[0].clone();
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        // Break in the middle: "one" | "two".
        let action = insert_page_break_action(&block, 3);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "page break replayed identically");
        assert_eq!(text_of(&a.doc, &block), "one", "head keeps the pre-caret text");
        let broke = body_blocks(&a.doc).into_iter().any(|x| {
            x.text == "two" && matches!(x.attrs.get("page_break_before"), Some(qdoc::AttrVal::Bool(true)))
        });
        assert!(broke, "the tail block starts on a fresh page");
    }

    /// Replacing the body (template apply / version restore) replays
    /// byte-identically: same fresh blocks on both replicas.
    #[test]
    fn replace_body_replays_byte_identical() {
        let (mut a, _ids) = seeded("Old content\n");
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let action = replace_body_action("# Fresh\n\nBrand new body\n");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(a.doc.get_heads(), b.doc.get_heads(), "body replace replayed identically");
        let first = body_blocks(&a.doc).into_iter().next().unwrap();
        assert_eq!(first.kind, "heading");
        assert_eq!(first.text, "Fresh");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action. Uses a paragraph-style
    /// change — a structural `kind` write the host's snapshot-diff undo reverts
    /// (inline formatting *marks* are, by design, not undo-tracked by the
    /// toolkit, so a bold toggle would not round-trip through Ctrl+Z).
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (mut state, ids) = seeded("Title\n");
        let block = ids[0].clone();
        let before_heads = state.doc.get_heads();

        let kind_of = |doc: &AutoCommit| {
            body_blocks(doc).into_iter().find(|x| x.id == block).map(|x| x.kind)
        };
        assert_eq!(kind_of(&state.doc).as_deref(), Some("paragraph"), "starts as a paragraph");

        let action = set_paragraph_style_action(vec![block.clone()], "heading", Some(1), None);
        dispatch(&mut state.doc, &action);
        state.record_committed_action(before_heads.clone(), None, None);

        assert_eq!(kind_of(&state.doc).as_deref(), Some("heading"), "the action made it a heading");
        assert!(state.peek_last_undo().is_some(), "exactly one undo boundary recorded");

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("paragraph"),
            "undo reverted the paragraph style and nothing else"
        );
    }

    /// Single source of truth: the palette's document-level specs encode
    /// byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (state, ids) = seeded("Intro\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block.clone(),
            caret_offset: 0,
            target_blocks: vec![block.clone()],
        };
        let resources = provide_resources(&ctx, &state.doc);
        let doc_res = &resources[0];
        assert_eq!(doc_res.kind, "word_processor.document");

        // Insert-table spec == insert_table builder.
        let spec = doc_res.actions.iter().find(|a| a.id == "insert-table").unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&insert_table_action(&block, 2, 2));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(from_spec.payload, from_builder.payload, "insert-table is one source of truth");

        // Align-center spec == set_align builder over the same target blocks.
        let spec = doc_res.actions.iter().find(|a| a.id == "align-center").unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&set_align_action(vec![block.clone()], "center"));
        assert_eq!(from_spec.payload, from_builder.payload, "align is one source of truth");
    }

    /// Ctrl+K publishes the document resource with its structural children: an
    /// image, a table and a heading each surface as their own resource with the
    /// expected actions. Drives the real resource-builder path.
    #[test]
    fn ctrl_k_publishes_document_and_structure() {
        // A document with a heading, an image and a table.
        let (mut state, ids) = seeded("# Section\n\nBody\n");
        let para = ids[1].clone();
        dispatch(
            &mut state.doc,
            &insert_image_action(&para, 0, vec![1, 2, 3, 4], Some(20), Some(10)),
        );
        let after_img = qdoc::model::blocks_in_order(&state.doc, &qdoc::model::open(&state.doc, &automerge::ROOT).unwrap());
        let last = after_img.last().cloned().unwrap();
        dispatch(&mut state.doc, &insert_table_action(&last, 2, 2));

        let ctx = WpResourceCtx {
            caret_block: para.clone(),
            caret_offset: 0,
            target_blocks: vec![para],
        };
        let resources = provide_resources(&ctx, &state.doc);
        assert_eq!(resources.len(), 1, "one document root");
        let root = &resources[0];
        let ids_of = |kind: &str| {
            root.children.iter().filter(|r| r.kind == kind).count()
        };
        assert_eq!(ids_of("word_processor.image"), 1, "an image resource");
        assert_eq!(ids_of("word_processor.table"), 1, "a table resource");
        assert!(ids_of("word_processor.section") >= 1, "a heading section resource");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Insert table")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Delete image")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Delete table")), "{labels:?}");
    }
}
