//! The word processor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation — a text splice, a formatting mark, a
//! paragraph attribute, a structural insert/delete — is a [`WordProcessorAction`]:
//! a small serializable value that deterministically mutates the shared `qdoc`
//! document. The same [`EncodedAction`] is what toolbar/panel/menu affordances
//! dispatch and what Ctrl+K dispatches — one source of truth, one replay path.
//! Presentation (caret, selection, scroll/zoom, open panel) stays out of the
//! document and never becomes an action.
//!
//! **Determinism contract**: [`Action::apply`] never reads the wall-clock or the
//! RNG. New block ids are minted *inside* `apply` from a deterministic
//! [`seeded_id_gen`](qdoc::model::seeded_id_gen); sets of blocks/spans are sorted
//! before iterating, so a replay emits the identical op sequence. A decoded
//! action with stale or hostile arguments is atomically refused before it can
//! write, so callers can surface a useful error without leaving a partial edit.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use qdoc::model::Region;
use qdoc::{Containers, DocMut};

use crate::num;

use qos_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec,
};

/// Registry `type_id` for every word-processor action.
pub const WORD_PROCESSOR_ACTION_TYPE: &str = "word-processor.action";
pub const WORD_PROCESSOR_OPERATION_TYPE: &str = "word-processor.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordProcessorOperation {
    Open { path: Option<String> },
    Save { path: Option<String> },
    SaveAs { path: Option<String> },
    Find,
    ZoomIn,
    ZoomOut,
    ToggleSpellcheck,
    ToggleMarkup,
    ToggleStyles,
    ToggleRuler,
    ToggleOutline,
    ToggleHistory,
}

/// Decode an application-owned word-processor operation payload.
///
/// # Errors
///
/// Returns a message when a payload bearing
/// [`WORD_PROCESSOR_OPERATION_TYPE`] is not valid JSON for
/// [`WordProcessorOperation`].
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<WordProcessorOperation>, String> {
    if type_id != WORD_PROCESSOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    value: &WordProcessorOperation,
) -> Option<ActionSpec> {
    let Ok(spec) = ActionSpec::operation(id, label, kind, WORD_PROCESSOR_OPERATION_TYPE, value)
    else {
        // A malformed operation must not become an invokable catalogue
        // entry. These variants contain only JSON-native values, but keep the
        // fallible serializer boundary fail-closed rather than panicking.
        return None;
    };
    match value {
        WordProcessorOperation::Open { .. }
        | WordProcessorOperation::Save { .. }
        | WordProcessorOperation::SaveAs { .. } => Some(
            spec.destructive()
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
        ),
        _ => Some(spec.with_contract(ActionContract::view())),
    }
}

/// Which page region a text/formatting op addresses. Mirrors [`qdoc::model::Region`]
/// (which is not itself serializable) so the region travels in the payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    #[default]
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
/// Deliberately positional: each variant names the exact block ids and offsets
/// it touches, so [`Action::apply`] is a pure, replayable write. Live per-
/// keystroke typing is *not* here (it splices directly into the CRDT `Text`); a
/// discrete text edit (Find & Replace, a scripted splice) is [`Self::SpliceText`].
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
    /// Replace one selection in document order.  Unlike a sequence of splices
    /// and deletes, this preserves the old editor's cross-paragraph merge as a
    /// single deterministic, atomic action/undo boundary.
    ReplaceSelection {
        region: RegionKind,
        start_block: String,
        start: usize,
        end_block: String,
        end: usize,
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
        #[serde(default)]
        region: RegionKind,
        blocks: Vec<String>,
        kind: String,
        level: Option<i64>,
        ordered: Option<bool>,
    },
    /// Apply a named paragraph style to `blocks`: set the `style` attr, and — for
    /// a built-in style — the structural `kind`/`level`/`ordered` too, as one
    /// action/undo boundary (not a kind write racing a separate style write).
    ApplyNamedStyle {
        #[serde(default)]
        region: RegionKind,
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
    InsertPageBreak { block: String, at: usize },
    /// Insert a fresh paragraph carrying `text` directly after block `after`.
    /// The action-level way to add a line: Enter is a live-edit path, not an
    /// action, so a headless participant (an agent, a script) had no way to
    /// append a paragraph with the same ops the app produces.
    InsertParagraph { after: String, text: String },
    /// Remove `blocks` (an image, or every cell of a table). Ids are sorted
    /// before removal so replicas emit the identical op sequence.
    RemoveBlocks { blocks: Vec<String> },
    /// Replace the whole document body with `markdown` (re-import a template /
    /// restore a version): every existing body block is removed, then the
    /// Markdown is imported as fresh blocks.
    ReplaceBody { markdown: String },
    /// Accept one tracked change, or every change when `id` is absent.
    AcceptChanges { id: Option<String> },
    /// Reject one tracked change, or every change when `id` is absent.
    RejectChanges { id: Option<String> },
    /// Resolve a comment by removing both its anchor mark and record.
    ResolveComment { id: String },
}

// Action payloads may arrive after the document has changed (or from an
// untrusted automation).  Keep the limits deliberately generous for real
// documents while making the action boundary a useful resource boundary too.
const MAX_ACTION_TEXT_CHARS: usize = 1_000_000;
const MAX_ACTION_BLOB_BYTES: usize = 64 * 1024 * 1024;
const MAX_TABLE_DIMENSION: usize = 100;
const MAX_TABLE_CELLS: usize = 10_000;

/// Validate an action against one materialized point-in-time view of `doc`.
///
/// This is intentionally separate from the write helpers: an invalid action
/// must make **no mutation**, never a partly-applied collection of valid
/// targets followed by a stale one. It also bounds hostile payloads before an
/// action allocates a large table/blob or asks automerge to index impossible
/// offsets.  The caller maps `false` to an explicit [`ActionError::Refused`],
/// preserving a useful stale-target result without leaving a partial edit.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive validation table keeps every action's atomic preconditions auditable at one boundary"
)]
fn action_is_valid(action: &WordProcessorAction, doc: &AutoCommit, c: &Containers) -> bool {
    let snap = qdoc::model::materialize(doc, c);
    let blocks_for = |region: RegionKind| match region {
        RegionKind::Body => &snap.blocks,
        RegionKind::Header => &snap.header,
        RegionKind::Footer => &snap.footer,
    };
    let block_len = |region: RegionKind, id: &str| {
        blocks_for(region)
            .iter()
            .find(|block| block.id == id)
            .map(|block| block.text.chars().count())
    };
    let unique_existing = |region: RegionKind, ids: &[String]| {
        !ids.is_empty()
            && ids.iter().all(|id| block_len(region, id).is_some())
            && ids.iter().collect::<HashSet<_>>().len() == ids.len()
    };
    let safe_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    let safe_text = |text: &str| text.chars().count() <= MAX_ACTION_TEXT_CHARS;
    let valid_attr = |key: &str, value: &AttrVal| match key {
        "align" => {
            matches!(value, AttrVal::Str(v) if matches!(v.as_str(), "left" | "center" | "right" | "justify"))
        }
        "page_break_before" | "flip_h" | "flip_v" => matches!(value, AttrVal::Bool(_)),
        "indent" | "first_line_indent" | "line_spacing" => {
            matches!(value, AttrVal::Float(v) if v.is_finite() && *v >= 0.0 && *v <= 10_000.0)
        }
        "tab_stops" | "style" => matches!(value, AttrVal::Str(v) if v.len() <= 4_096),
        "width" | "height" => matches!(value, AttrVal::Int(v) if *v > 0 && *v <= 100_000),
        _ => false,
    };

    match action {
        WordProcessorAction::SpliceText {
            region,
            block,
            at,
            del,
            ins,
        } => {
            block_len(*region, block)
                .is_some_and(|len| *at <= len && *del <= len.saturating_sub(*at))
                && safe_text(ins)
        }
        WordProcessorAction::ReplaceSelection {
            region,
            start_block,
            start,
            end_block,
            end,
            ins,
        } => {
            let blocks = blocks_for(*region);
            let start_index = blocks.iter().position(|block| block.id == *start_block);
            let end_index = blocks.iter().position(|block| block.id == *end_block);
            match (
                start_index,
                end_index,
                block_len(*region, start_block),
                block_len(*region, end_block),
            ) {
                (Some(start_index), Some(end_index), Some(start_len), Some(end_len)) => {
                    (start_index < end_index || (start_index == end_index && start <= end))
                        && *start <= start_len
                        && *end <= end_len
                        && safe_text(ins)
                }
                _ => false,
            }
        }
        WordProcessorAction::SetMark {
            region,
            spans,
            name,
            value,
            also_clear,
        } => {
            safe_name(name)
                && also_clear.iter().all(|sibling| safe_name(sibling))
                && spans.len() <= 10_000
                && !spans.is_empty()
                && spans.iter().all(|span| {
                    block_len(*region, &span.block)
                        .is_some_and(|len| span.start < span.end && span.end <= len)
                })
                && match value {
                    Some(MarkVal::Str(text)) => safe_text(text),
                    _ => true,
                }
        }
        WordProcessorAction::SetParagraphStyle {
            region,
            blocks,
            kind,
            level,
            ..
        } => {
            unique_existing(*region, blocks)
                && safe_name(kind)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        WordProcessorAction::ApplyNamedStyle {
            region,
            blocks,
            style,
            kind,
            level,
            ..
        } => {
            unique_existing(*region, blocks)
                && !style.is_empty()
                && style.len() <= 128
                && kind.as_deref().is_none_or(safe_name)
                && level.is_none_or(|level| (0..=9).contains(&level))
        }
        WordProcessorAction::SetBlockAttr {
            region,
            blocks,
            key,
            value,
        } => unique_existing(*region, blocks) && valid_attr(key, value),
        WordProcessorAction::InsertTable { after, rows, cols } => {
            block_len(RegionKind::Body, after).is_some()
                && *rows > 0
                && *cols > 0
                && *rows <= MAX_TABLE_DIMENSION
                && *cols <= MAX_TABLE_DIMENSION
                && rows.saturating_mul(*cols) <= MAX_TABLE_CELLS
        }
        WordProcessorAction::InsertImage {
            block,
            at,
            bytes,
            width,
            height,
        } => {
            block_len(RegionKind::Body, block).is_some_and(|len| *at <= len)
                && !bytes.is_empty()
                && bytes.len() <= MAX_ACTION_BLOB_BYTES
                && width.is_none_or(|value| (1..=100_000).contains(&value))
                && height.is_none_or(|value| (1..=100_000).contains(&value))
        }
        WordProcessorAction::InsertPageBreak { block, at } => {
            block_len(RegionKind::Body, block).is_some_and(|len| *at <= len)
        }
        WordProcessorAction::InsertParagraph { after, text } => {
            block_len(RegionKind::Body, after).is_some() && safe_text(text)
        }
        WordProcessorAction::RemoveBlocks { blocks } => unique_existing(RegionKind::Body, blocks),
        WordProcessorAction::ReplaceBody { markdown } => safe_text(markdown),
        WordProcessorAction::AcceptChanges { id } | WordProcessorAction::RejectChanges { id } => id
            .as_ref()
            .is_none_or(|id| snap.changes.iter().any(|change| &change.id == id)),
        WordProcessorAction::ResolveComment { id } => {
            !id.is_empty() && snap.comments.iter().any(|comment| comment.id == *id)
        }
    }
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
            MarkVal::Str(s) => {
                dm.add_mark_in(r, &span.block, span.start, span.end, name, s.as_str());
            }
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
    if let Some(container) = region_container_obj(dm.doc, c, region)
        && let Some(obj) = region_block_obj(dm.doc, &container, &span.block)
        && let Ok(Some((automerge::Value::Object(automerge::ObjType::Text), tid))) =
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

fn set_kind_region(dm: &mut DocMut, region: Region, block: &str, kind: &str) {
    if region == Region::Body {
        dm.set_kind(block, kind);
    } else if let Some(container) = region_container_obj(dm.doc, dm.c, region)
        && let Some(block_obj) = region_block_obj(dm.doc, &container, block)
    {
        let _ = dm.doc.put(&block_obj, qdoc::schema::BLOCK_KIND, kind);
    }
}

// ---------------------------------------------------------------------------
// Per-variant `apply` bodies. Split out of `Action::apply` below (one match
// arm each) so that single trait method stays short; kept as free functions
// (not methods on `WordProcessorAction`) because they live outside the trait
// impl. Each takes the `dm: &mut DocMut` the caller builds once (`doc`/`c`/
// `ids` bundled), matching the region helpers above; `dm.c` stands in for a
// separate `c` parameter where one is needed.
// ---------------------------------------------------------------------------

fn apply_splice_text(
    dm: &mut DocMut,
    region: Region,
    block: &str,
    at: usize,
    del: usize,
    ins: &str,
) {
    dm.splice_text_in(region, block, at, num::isize_of_usize(del), ins);
}

fn remove_block_in_region(dm: &mut DocMut, region: Region, block: &str) {
    if region == Region::Body {
        dm.remove_block(block);
    } else if let Some(container) = region_container_obj(dm.doc, dm.c, region) {
        let _ = dm.doc.delete(&container, block);
    }
}

fn apply_replace_selection(
    dm: &mut DocMut,
    region: Region,
    start_block: &str,
    start: usize,
    end_block: &str,
    end: usize,
    ins: &str,
) {
    if start_block == end_block {
        dm.splice_text_in(
            region,
            start_block,
            start,
            num::isize_of_usize(end - start),
            ins,
        );
        return;
    }
    // Validation has already established both block positions/offsets against
    // one snapshot, so this body cannot partly merge one valid end-point and
    // then discover a stale second end-point.
    let blocks = match region {
        Region::Body => qdoc::model::blocks_in_order(dm.doc, dm.c),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .map(|block| block.id)
            .collect(),
    };
    let Some(start_index) = blocks.iter().position(|block| block == start_block) else {
        return;
    };
    let Some(end_index) = blocks.iter().position(|block| block == end_block) else {
        return;
    };
    let end_text = match region {
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, end_block),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .find(|block| block.id == end_block)
            .map(|block| block.text),
    }
    .unwrap_or_default();
    let tail: String = end_text.chars().skip(end).collect();
    let start_len = match region {
        Region::Body => qdoc::model::block_text(dm.doc, dm.c, start_block),
        region => qdoc::model::materialize_region(dm.doc, dm.c, region)
            .into_iter()
            .find(|block| block.id == start_block)
            .map(|block| block.text),
    }
    .map_or(0, |text| text.chars().count());
    dm.splice_text_in(
        region,
        start_block,
        start,
        num::isize_of_usize(start_len.saturating_sub(start)),
        ins,
    );
    if !tail.is_empty() {
        dm.splice_text_in(region, start_block, start + ins.chars().count(), 0, &tail);
    }
    if let Some(to_remove) = blocks.get(start_index.saturating_add(1)..=end_index) {
        for block in to_remove {
            remove_block_in_region(dm, region, block);
        }
    }
}

fn apply_set_mark(
    dm: &mut DocMut,
    region: Region,
    spans: &[Span],
    name: &str,
    value: Option<&MarkVal>,
    also_clear: &[String],
) {
    let c = dm.c;
    let mut sorted = spans.to_vec();
    sorted.sort_by(|a, b| {
        (a.block.as_str(), a.start, a.end).cmp(&(b.block.as_str(), b.start, b.end))
    });
    for span in &sorted {
        if span.start >= span.end {
            continue;
        }
        for sib in also_clear {
            remove_mark_region(dm, c, region, span, sib);
        }
        match value {
            None => remove_mark_region(dm, c, region, span, name),
            Some(v) => {
                // Replace: drop any prior value of the same mark before
                // re-adding so values overwrite, not stack.
                remove_mark_region(dm, c, region, span, name);
                add_mark_region(dm, region, span, name, v);
            }
        }
    }
}

fn apply_set_paragraph_style(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        set_kind_region(dm, region, block, kind);
        if let Some(l) = level {
            set_attr_region(dm, region, block, "level", &AttrVal::Int(l));
        }
        if let Some(o) = ordered {
            set_attr_region(dm, region, block, "ordered", &AttrVal::Bool(o));
        }
    }
}

fn apply_named_style(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    style: &str,
    kind: Option<&String>,
    level: Option<i64>,
    ordered: Option<bool>,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        if let Some(k) = kind {
            set_kind_region(dm, region, block, k);
            if let Some(l) = level {
                set_attr_region(dm, region, block, "level", &AttrVal::Int(l));
            }
            if let Some(o) = ordered {
                set_attr_region(dm, region, block, "ordered", &AttrVal::Bool(o));
            }
        }
        set_attr_region(dm, region, block, "style", &AttrVal::Str(style.to_string()));
    }
}

fn apply_set_block_attr(
    dm: &mut DocMut,
    region: Region,
    blocks: &[String],
    key: &str,
    value: &AttrVal,
) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        set_attr_region(dm, region, block, key, value);
    }
}

fn apply_insert_table(dm: &mut DocMut, after: &str, rows: usize, cols: usize) {
    let table_id = dm.ids.mint(); // one id shared by every cell
    let mut prev = Some(after.to_string());
    for row in 0..rows {
        for col in 0..cols {
            let id = dm.insert_block(prev.as_deref(), "cell");
            dm.set_attr(&id, "table", table_id.as_str());
            dm.set_attr(&id, "row", num::i64_of_usize(row));
            dm.set_attr(&id, "col", num::i64_of_usize(col));
            prev = Some(id);
        }
    }
}

fn apply_insert_image(
    dm: &mut DocMut,
    block: &str,
    at: usize,
    bytes: &[u8],
    width: Option<i64>,
    height: Option<i64>,
) {
    let text = qdoc::model::block_text(dm.doc, dm.c, block).unwrap_or_default();
    let char_count = text.chars().count();
    let tail: String = text.chars().skip(at).collect();
    let hash = dm.put_blob(bytes);
    if let Some(tail) = char_count.checked_sub(at).filter(|n| *n > 0) {
        dm.splice_text(block, at, num::isize_of_usize(tail), "");
    }
    let img = dm.insert_block(Some(block), "image");
    dm.set_attr(&img, "blob", hash.as_str());
    if let Some(w) = width {
        dm.set_attr(&img, "width", w);
    }
    if let Some(h) = height {
        dm.set_attr(&img, "height", h);
    }
    let after = dm.insert_block(Some(&img), "paragraph");
    if !tail.is_empty() {
        dm.splice_text(&after, 0, 0, &tail);
    }
}

fn apply_insert_page_break(dm: &mut DocMut, block: &str, at: usize) {
    // Split the block at the caret (like Enter): the tail moves into a fresh
    // paragraph that starts on a new page.
    let text = qdoc::model::block_text(dm.doc, dm.c, block).unwrap_or_default();
    let char_count = text.chars().count();
    let tail: String = text.chars().skip(at).collect();
    if let Some(tail) = char_count.checked_sub(at).filter(|n| *n > 0) {
        dm.splice_text(block, at, num::isize_of_usize(tail), "");
    }
    let new = dm.insert_block(Some(block), "paragraph");
    if !tail.is_empty() {
        dm.splice_text(&new, 0, 0, &tail);
    }
    dm.set_attr(&new, "page_break_before", true);
}

fn apply_insert_paragraph(dm: &mut DocMut, after: &str, text: &str) {
    let new = dm.insert_block(Some(after), "paragraph");
    if !text.is_empty() {
        dm.splice_text(&new, 0, 0, text);
    }
}

fn apply_remove_blocks(dm: &mut DocMut, blocks: &[String]) {
    let mut sorted = blocks.to_vec();
    sorted.sort();
    for block in &sorted {
        dm.remove_block(block);
    }
}

fn apply_replace_body(dm: &mut DocMut, markdown: &str) {
    use qdoc::FormatProvider;
    let existing = qdoc::model::blocks_in_order(dm.doc, dm.c);
    for id in &existing {
        dm.remove_block(id);
    }
    // A malformed template can't wedge a replay: the body-clear stays in the
    // pinned change either way.
    let _ = qdoc::MarkdownCodec.import(markdown.as_bytes(), dm);
}

fn ordered_change_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    let order = qdoc::model::blocks_in_order(doc, c);
    let mut ids = qdoc::model::change_ids(doc, c);
    ids.sort_by_key(|id| {
        qdoc::model::change_range(doc, c, id).map_or_else(
            || (usize::MAX, usize::MAX, id.clone()),
            |(block, start, _)| {
                (
                    order
                        .iter()
                        .position(|candidate| candidate == &block)
                        .unwrap_or(usize::MAX),
                    start,
                    id.clone(),
                )
            },
        )
    });
    ids
}

fn apply_review_changes(dm: &mut DocMut, id: Option<&str>, accept: bool) {
    let mut ids = id.map_or_else(
        || ordered_change_ids(dm.doc, dm.c),
        |one| vec![one.to_string()],
    );
    // Work from the end of the document so removing text cannot shift an
    // earlier change's anchored range before it is processed.
    ids.reverse();
    for id in &ids {
        if accept {
            dm.accept_change(id);
        } else {
            dm.reject_change(id);
        }
    }
}

fn apply_resolve_comment(dm: &mut DocMut, id: &str) {
    let range = qdoc::model::comment_range(dm.doc, dm.c, id);
    if let Some((block, start, end)) = range
        && start < end
    {
        dm.remove_mark(&block, start, end, qdoc::schema::MARK_COMMENT);
    }
    if let Some(comments) = dm.c.comments.as_ref() {
        let _ = dm.doc.delete(comments, id);
    }
}

impl Action for WordProcessorAction {
    /// Apply this action's mutation to `doc`.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Refused`] for an unavailable Writer document or
    /// stale/hostile decoded arguments before opening a mutable view.
    #[allow(
        clippy::too_many_lines,
        reason = "one match deliberately makes the complete action envelope surface reviewable in one place"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Bind to the canonical schema under ROOT. Refuse an unbound document
        // explicitly so callers never mistake an unavailable target for a
        // successful edit.
        let Ok(c) = qdoc::model::open(doc, &automerge::ROOT) else {
            return Err(ActionError::Refused(
                "Writer document is unavailable or has not been initialized".into(),
            ));
        };
        // Validate every target against this one, immutable document state
        // before creating the mutable editing view.  In particular, do not let
        // a stale multi-block format/delete partially affect still-existing
        // blocks when one peer has already removed another target.
        if !action_is_valid(self, doc, &c) {
            return Err(ActionError::Refused(
                "action targets are stale or its arguments are outside Writer's safe limits".into(),
            ));
        }
        // A generator seeded past every existing id, under the executor's pinned
        // actor — so any freshly-minted block id is `{action-actor}.{n}`,
        // byte-identical on origin and replay.
        let mut ids = qdoc::model::seeded_id_gen(doc, &c);
        let mut dm = DocMut {
            doc,
            c: &c,
            ids: &mut ids,
        };

        match self {
            WordProcessorAction::SpliceText {
                region,
                block,
                at,
                del,
                ins,
            } => apply_splice_text(&mut dm, region.to_region(), block, *at, *del, ins),

            WordProcessorAction::ReplaceSelection {
                region,
                start_block,
                start,
                end_block,
                end,
                ins,
            } => apply_replace_selection(
                &mut dm,
                region.to_region(),
                start_block,
                *start,
                end_block,
                *end,
                ins,
            ),

            WordProcessorAction::SetMark {
                region,
                spans,
                name,
                value,
                also_clear,
            } => apply_set_mark(
                &mut dm,
                region.to_region(),
                spans,
                name,
                value.as_ref(),
                also_clear,
            ),

            WordProcessorAction::SetParagraphStyle {
                region,
                blocks,
                kind,
                level,
                ordered,
            } => apply_set_paragraph_style(
                &mut dm,
                region.to_region(),
                blocks,
                kind,
                *level,
                *ordered,
            ),

            WordProcessorAction::ApplyNamedStyle {
                region,
                blocks,
                style,
                kind,
                level,
                ordered,
            } => apply_named_style(
                &mut dm,
                region.to_region(),
                blocks,
                style,
                kind.as_ref(),
                *level,
                *ordered,
            ),

            WordProcessorAction::SetBlockAttr {
                region,
                blocks,
                key,
                value,
            } => apply_set_block_attr(&mut dm, region.to_region(), blocks, key, value),

            WordProcessorAction::InsertTable { after, rows, cols } => {
                apply_insert_table(&mut dm, after, *rows, *cols);
            }

            WordProcessorAction::InsertImage {
                block,
                at,
                bytes,
                width,
                height,
            } => apply_insert_image(&mut dm, block, *at, bytes, *width, *height),

            WordProcessorAction::InsertPageBreak { block, at } => {
                apply_insert_page_break(&mut dm, block, *at);
            }

            WordProcessorAction::InsertParagraph { after, text } => {
                apply_insert_paragraph(&mut dm, after, text);
            }

            WordProcessorAction::RemoveBlocks { blocks } => {
                apply_remove_blocks(&mut dm, blocks);
            }

            WordProcessorAction::ReplaceBody { markdown } => {
                apply_replace_body(&mut dm, markdown);
            }

            WordProcessorAction::AcceptChanges { id } => {
                apply_review_changes(&mut dm, id.as_deref(), true);
            }

            WordProcessorAction::RejectChanges { id } => {
                apply_review_changes(&mut dm, id.as_deref(), false);
            }

            WordProcessorAction::ResolveComment { id } => {
                apply_resolve_comment(&mut dm, id);
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
    // The action is a plain serde enum over owned data, so the JSON encode
    // cannot fail. Were it ever to, an empty payload fails decoding at the far
    // end as an explicit action error instead of aborting the process.
    EncodedAction::encode(WORD_PROCESSOR_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: WORD_PROCESSOR_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
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

/// Apply one external or internal hyperlink target to the selected text.
///
/// The URL remains a normal valued `link` mark, so it uses the same safe
/// export/import/collaboration path as imported HTML/ODT/DOCX links. A target
/// beginning with `#` is a document-local bookmark/cross-reference.
pub fn set_hyperlink_action(
    region: RegionKind,
    spans: Vec<Span>,
    target: &str,
) -> WordProcessorAction {
    set_mark_action(
        region,
        spans,
        "link",
        Some(MarkVal::Str(target.trim().to_string())),
        Vec::new(),
    )
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
    set_block_attr_action(
        RegionKind::Body,
        blocks,
        "align",
        AttrVal::Str(align.to_string()),
    )
}

/// Apply a named paragraph style (and, for a built-in, its structural kind) to
/// `blocks` in one action.
pub fn apply_named_style_action(
    region: RegionKind,
    blocks: Vec<String>,
    style: &str,
    kind: Option<&str>,
    level: Option<i64>,
    ordered: Option<bool>,
) -> WordProcessorAction {
    WordProcessorAction::ApplyNamedStyle {
        region,
        blocks,
        style: style.to_string(),
        kind: kind.map(std::string::ToString::to_string),
        level,
        ordered,
    }
}

/// Set the paragraph kind/level/ordered on `blocks`.
pub fn set_paragraph_style_action(
    region: RegionKind,
    blocks: Vec<String>,
    kind: &str,
    level: Option<i64>,
    ordered: Option<bool>,
) -> WordProcessorAction {
    WordProcessorAction::SetParagraphStyle {
        region,
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
/// Insert a paragraph carrying `text` after block `after`.
pub fn insert_paragraph_action(after: &str, text: &str) -> WordProcessorAction {
    WordProcessorAction::InsertParagraph {
        after: after.to_string(),
        text: text.to_string(),
    }
}

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

/// Replace a selection that may span several blocks with one canonical action.
pub fn replace_selection_action(
    region: RegionKind,
    start_block: &str,
    start: usize,
    end_block: &str,
    end: usize,
    text: &str,
) -> WordProcessorAction {
    WordProcessorAction::ReplaceSelection {
        region,
        start_block: start_block.to_string(),
        start,
        end_block: end_block.to_string(),
        end,
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
    /// Exact non-empty character spans selected at palette invocation time.
    /// This keeps a prompted hyperlink action resource-addressed and replayable
    /// instead of consulting mutable presentation state after the user enters
    /// a target.
    pub format_spans: Vec<Span>,
    /// The selection in document order when it is non-empty.  This semantic
    /// context, rather than a caller-provided block id, binds the palette's
    /// Replace selection operation to exactly what the user has selected.
    pub selection: Option<SelectionTarget>,
    pub region: RegionKind,
}

/// One non-empty selection expressed as stable block ids and character offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionTarget {
    pub start_block: String,
    pub start: usize,
    pub end_block: String,
    pub end: usize,
}

fn immediate_spec(id: &str, label: &str, action: &WordProcessorAction) -> ActionSpec {
    // Built from `encode`, which is total, rather than the fallible
    // `ActionSpec::immediate` — the two produce the same spec.
    //
    // Through the constructor rather than a struct literal: `ActionSpec`
    // carries private `params`/`build` fields, so a literal cannot name
    // them and the shared defaults would be bypassed if it could.
    let spec =
        ActionSpec::immediate_encoded(id, label, action.locus(), encode(action)).reversible();
    if id.contains("delete") || id.contains("remove") {
        spec.destructive()
    } else {
        spec
    }
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
#[allow(
    clippy::too_many_lines,
    reason = "the resource catalogue is one ordered declaration of the live document surface"
)]
pub fn provide_resources(ctx: &WpResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new(
        "word-processor.document",
        "word_processor.document",
        "Document",
    );

    for (id, label, variant) in [
        ("open", "Open document", 0_u8),
        ("save", "Save document", 1_u8),
        ("save-as", "Save document as", 2_u8),
    ] {
        root = root.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system file picker)",
                OperationKind::Effect,
                WORD_PROCESSOR_OPERATION_TYPE,
                vec![ActionParam::optional_owned(
                    "path",
                    ParamType::Text,
                    "Direct filesystem path; omit to open the system file picker",
                )],
                move |args: &ActionArgs| {
                    let path = args
                        .text("path")
                        .filter(|path| !path.trim().is_empty())
                        .map(str::to_string);
                    let operation = match variant {
                        0 => WordProcessorOperation::Open { path },
                        1 => WordProcessorOperation::Save { path },
                        _ => WordProcessorOperation::SaveAs { path },
                    };
                    EncodedAction::encode_operation(WORD_PROCESSOR_OPERATION_TYPE, &operation)
                        .unwrap_or_else(|_| EncodedAction {
                            type_id: WORD_PROCESSOR_OPERATION_TYPE.into(),
                            payload: Vec::new(),
                        })
                },
            )
            .destructive()
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        );
    }

    for (id, label, kind, value) in [
        (
            "find",
            "Find in document",
            OperationKind::View,
            WordProcessorOperation::Find,
        ),
        (
            "zoom-in",
            "Zoom in",
            OperationKind::View,
            WordProcessorOperation::ZoomIn,
        ),
        (
            "zoom-out",
            "Zoom out",
            OperationKind::View,
            WordProcessorOperation::ZoomOut,
        ),
        (
            "spellcheck",
            "Toggle spellcheck",
            OperationKind::View,
            WordProcessorOperation::ToggleSpellcheck,
        ),
        (
            "markup",
            "Toggle review markup",
            OperationKind::View,
            WordProcessorOperation::ToggleMarkup,
        ),
        (
            "styles",
            "Toggle styles panel",
            OperationKind::View,
            WordProcessorOperation::ToggleStyles,
        ),
        (
            "ruler",
            "Toggle ruler",
            OperationKind::View,
            WordProcessorOperation::ToggleRuler,
        ),
        (
            "outline",
            "Toggle outline",
            OperationKind::View,
            WordProcessorOperation::ToggleOutline,
        ),
        (
            "history",
            "Toggle history",
            OperationKind::View,
            WordProcessorOperation::ToggleHistory,
        ),
    ] {
        if let Some(spec) = operation(id, label, kind, &value) {
            root = root.with_action(spec);
        }
    }

    // Document-level structural inserts, anchored on the caret.
    root = root.with_action(immediate_spec(
        "insert-table",
        "Insert table",
        &insert_table_action(&ctx.caret_block, 2, 2),
    ));
    if !ctx.format_spans.is_empty() {
        let spans = ctx.format_spans.clone();
        let region = ctx.region;
        root = root.with_action(ActionSpec::typed(
            "set-hyperlink",
            "Add or change hyperlink",
            "Web URL (https://… or mailto:…) or #bookmark",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "target",
                ParamType::Text,
                "A safe external URL, mail address, or document-local #bookmark target",
            )],
            move |args: &ActionArgs| {
                let target = args.text("target").unwrap_or_default().trim();
                if target.is_empty() {
                    return EncodedAction {
                        type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"hyperlink target is required".to_vec(),
                    };
                }
                encode(&set_hyperlink_action(region, spans.clone(), target))
            },
        ));
    }
    if let Some(selection) = &ctx.selection {
        let selection = selection.clone();
        let region = ctx.region;
        root = root.with_action(ActionSpec::typed(
            "replace-selection",
            "Replace selection",
            "Replacement text for the selected document range",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required_owned(
                "text",
                ParamType::Text,
                "Text that replaces the current selection (empty deletes it)",
            )],
            move |args: &ActionArgs| {
                let Some(text) = args.text("text") else {
                    return EncodedAction {
                        type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: b"replacement text is required".to_vec(),
                    };
                };
                encode(&replace_selection_action(
                    region,
                    &selection.start_block,
                    selection.start,
                    &selection.end_block,
                    selection.end,
                    text,
                ))
            },
        ));
    }
    let image_anchor = ctx.caret_block.clone();
    let image_offset = ctx.caret_offset;
    root = root.with_action(
        ActionSpec::prompted_content(
            "place-image",
            "Place image",
            "Image content produced by the preceding action",
            "content hash",
            ExecutionLocus::ClientLocal,
            move |hash| {
                let Some(bytes) = qdoc::blobstore::get(hash) else {
                    return EncodedAction {
                        type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: format!("content `{hash}` is not available on this machine")
                            .into_bytes(),
                    };
                };
                let meta = qdoc::blobstore::sniff(&bytes);
                encode(&insert_image_action(
                    &image_anchor,
                    image_offset,
                    bytes,
                    (meta.w > 0).then(|| i64::from(meta.w)),
                    (meta.h > 0).then(|| i64::from(meta.h)),
                ))
            },
        )
        .with_description(
            "Place the single content result of the immediately preceding action at the caret",
        )
        .reversible(),
    );
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

    // Review is document state, not an overlay-only island: comments and
    // tracked changes are navigable resources, and every resolution decision
    // is the same typed, replayable action used by the margin cards.
    if !snap.comments.is_empty() || !snap.changes.is_empty() {
        let mut review = Resource::new("word-processor.review", "word_processor.review", "Review");
        if !snap.changes.is_empty() {
            review = review
                .with_action(immediate_spec(
                    "accept-all",
                    "Accept all changes",
                    &WordProcessorAction::AcceptChanges { id: None },
                ))
                .with_action(immediate_spec(
                    "reject-all",
                    "Reject all changes",
                    &WordProcessorAction::RejectChanges { id: None },
                ));
        }
        for comment in &snap.comments {
            let text = comment
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let label = format!(
                "Comment by {}: {}",
                short(&comment.author, "Unknown author"),
                short(&text, "Empty comment")
            );
            review = review.with_child(
                Resource::new(
                    format!("word-processor.comment:{}", comment.id),
                    "word_processor.comment",
                    label,
                )
                .with_action(immediate_spec(
                    "resolve",
                    "Resolve comment",
                    &WordProcessorAction::ResolveComment {
                        id: comment.id.clone(),
                    },
                )),
            );
        }
        for change in &snap.changes {
            let kind = match change.kind {
                qdoc::snapshot::ChangeKind::Ins => "Insertion",
                qdoc::snapshot::ChangeKind::Del => "Deletion",
            };
            review = review.with_child(
                Resource::new(
                    format!("word-processor.change:{}", change.id),
                    "word_processor.change",
                    format!("{kind} by {}", short(&change.author, "Unknown author")),
                )
                .with_action(immediate_spec(
                    "accept",
                    "Accept change",
                    &WordProcessorAction::AcceptChanges {
                        id: Some(change.id.clone()),
                    },
                ))
                .with_action(immediate_spec(
                    "reject",
                    "Reject change",
                    &WordProcessorAction::RejectChanges {
                        id: Some(change.id.clone()),
                    },
                )),
            );
        }
        root = root.with_child(review);
    }

    if !snap.notes.is_empty() {
        let mut notes = Resource::new("word-processor.notes", "word_processor.notes", "Notes");
        for note in &snap.notes {
            let text = note
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            notes = notes.with_child(Resource::new(
                format!("word-processor.note:{}", note.id),
                "word_processor.note",
                format!("{}: {}", note.kind, short(&text, "Empty note")),
            ));
        }
        root = root.with_child(notes);
    }

    if !snap.bookmarks.is_empty() {
        let mut bookmarks = Resource::new(
            "word-processor.bookmarks",
            "word_processor.bookmarks",
            "Bookmarks",
        );
        for bookmark in &snap.bookmarks {
            bookmarks = bookmarks.with_child(Resource::new(
                format!("word-processor.bookmark:{}", bookmark.name),
                "word_processor.bookmark",
                bookmark.name.clone(),
            ));
        }
        root = root.with_child(bookmarks);
    }

    if !snap.styles.is_empty() {
        let mut styles = Resource::new(
            "word-processor.styles",
            "word_processor.styles",
            "Paragraph styles",
        );
        for style in snap.styles.keys() {
            styles = styles.with_child(Resource::new(
                format!("word-processor.style:{style}"),
                "word_processor.style",
                style.clone(),
            ));
        }
        root = root.with_child(styles);
    }

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
        if let Some(qdoc::AttrVal::Str(tid)) = b.attrs.get("table")
            && !table_ids.contains(tid)
        {
            table_ids.push(tid.clone());
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
        match self.wp_resource_ctx(doc) {
            Some(ctx) => provide_resources(&ctx, doc),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::ActionInput;
    use qos_actions::LocalTransport;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
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
            qdoc::MarkdownCodec
                .import(markdown.as_bytes(), &mut dm)
                .unwrap();
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
        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "precondition: same base state"
        );

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
            vec![Span {
                block: block.clone(),
                start: 0,
                end: 5,
            }],
            "strong",
            Some(MarkVal::Bool(true)),
            vec![],
        );
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "mark replayed identically"
        );
        let marks = body_blocks(&b.doc)
            .into_iter()
            .find(|x| x.id == block)
            .map(|x| x.marks)
            .unwrap_or_default();
        assert!(
            marks
                .iter()
                .any(|m| m.name == "strong" && m.start == 0 && m.end == 5),
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

        let action = set_paragraph_style_action(
            RegionKind::Body,
            vec![block.clone()],
            "heading",
            Some(1),
            None,
        );
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &env, &registry()).unwrap();

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "style replayed identically"
        );
        assert_eq!(
            body_blocks(&b.doc)
                .into_iter()
                .find(|x| x.id == block)
                .unwrap()
                .kind,
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

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "table replayed identically"
        );
        let cells_a = body_blocks(&a.doc)
            .into_iter()
            .filter(|x| x.kind == "cell")
            .count();
        let cells_b = body_blocks(&b.doc)
            .into_iter()
            .filter(|x| x.kind == "cell")
            .count();
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

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "page break replayed identically"
        );
        assert_eq!(
            text_of(&a.doc, &block),
            "one",
            "head keeps the pre-caret text"
        );
        let broke = body_blocks(&a.doc).into_iter().any(|x| {
            x.text == "two"
                && matches!(
                    x.attrs.get("page_break_before"),
                    Some(qdoc::AttrVal::Bool(true))
                )
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

        assert_eq!(
            a.doc.get_heads(),
            b.doc.get_heads(),
            "body replace replayed identically"
        );
        let first = body_blocks(&a.doc).into_iter().next().unwrap();
        assert_eq!(first.kind, "heading");
        assert_eq!(first.text, "Fresh");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, reverted
    /// precisely by Ctrl+Z. Uses a paragraph-style change (a structural `kind`
    /// write) since inline formatting *marks* are, by design, not undo-tracked.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (mut state, ids) = seeded("Title\n");
        let block = ids[0].clone();
        let before_heads = state.doc.get_heads();

        let kind_of = |doc: &AutoCommit| {
            body_blocks(doc)
                .into_iter()
                .find(|x| x.id == block)
                .map(|x| x.kind)
        };
        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("paragraph"),
            "starts as a paragraph"
        );

        let action = set_paragraph_style_action(
            RegionKind::Body,
            vec![block.clone()],
            "heading",
            Some(1),
            None,
        );
        let result = action.apply(&mut state.doc);
        assert!(result.is_ok());
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            kind_of(&state.doc).as_deref(),
            Some("heading"),
            "the action made it a heading"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

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
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
        };
        let resources = provide_resources(&ctx, &state.doc);
        let doc_res = &resources[0];
        assert_eq!(doc_res.kind, "word_processor.document");

        // Insert-table spec == insert_table builder.
        let spec = doc_res
            .actions
            .iter()
            .find(|a| a.id == "insert-table")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&insert_table_action(&block, 2, 2));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "insert-table is one source of truth"
        );

        // Align-center spec == set_align builder over the same target blocks.
        let spec = doc_res
            .actions
            .iter()
            .find(|a| a.id == "align-center")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&set_align_action(vec![block], "center"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "align is one source of truth"
        );
    }

    #[test]
    fn selected_text_exposes_a_typed_hyperlink_action_that_replays() {
        let (mut state, ids) = seeded("Read this\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            format_spans: vec![Span {
                block: block.clone(),
                start: 0,
                end: 4,
            }],
            selection: Some(SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block.clone(),
                end: 4,
            }),
            region: RegionKind::Body,
            caret_block: block.clone(),
            caret_offset: 4,
            target_blocks: vec![block.clone()],
        };
        let root = provide_resources(&ctx, &state.doc).pop().unwrap();
        let replace = root
            .actions
            .iter()
            .find(|action| action.id == "replace-selection")
            .expect("selected text exposes canonical replacement");
        let replacement = replace
            .build_args([(
                "text".to_string(),
                qos_actions::ArgValue::Text("guide".into()),
            )])
            .unwrap();
        assert_eq!(
            replacement,
            encode(&replace_selection_action(
                RegionKind::Body,
                &block,
                0,
                &block,
                4,
                "guide",
            )),
            "palette binding is byte-identical to the canonical selection action"
        );
        let link = root
            .actions
            .iter()
            .find(|action| action.id == "set-hyperlink")
            .expect("selected text exposes link authoring");
        assert_eq!(link.params().len(), 1);
        assert_eq!(link.params()[0].name, "target");
        let encoded = link
            .build_args([(
                "target".to_string(),
                qos_actions::ArgValue::Text("https://example.com/guide".into()),
            )])
            .unwrap();
        let action: WordProcessorAction = serde_json::from_slice(&encoded.payload).unwrap();
        action.apply(&mut state.doc).unwrap();
        assert!(body_blocks(&state.doc)[0].marks.iter().any(|mark| {
            mark.name == "link"
                && mark.start == 0
                && mark.end == 4
                && mark.value.as_str() == Some("https://example.com/guide")
        }));
    }

    #[test]
    fn file_operations_are_typed_background_jobs() {
        let (state, ids) = seeded("Intro\n");
        let ctx = WpResourceCtx {
            caret_block: ids[0].clone(),
            caret_offset: 0,
            target_blocks: vec![ids[0].clone()],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
        };
        let resources = provide_resources(&ctx, &state.doc);
        for id in ["open", "save", "save-as"] {
            let action = resources[0]
                .actions
                .iter()
                .find(|action| action.id == id)
                .expect("file action");
            assert_eq!(action.contract.result, ResultSpec::Job);
            assert!(matches!(
                action.contract.execution,
                ExecutionMode::Background { .. }
            ));
            assert!(action.contract.validate_declaration().is_empty());
        }
    }

    /// The hand-off operation consumes bytes an earlier action actually put
    /// in the content store. The model supplies neither the hash nor image
    /// metadata; the app resolves both while building its own document action.
    #[test]
    fn place_image_consumes_a_published_content_hash() {
        let (mut state, ids) = seeded("Intro\n");
        let block = ids[0].clone();
        let ctx = WpResourceCtx {
            caret_block: block,
            caret_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
        };
        let bytes = b"content supplied by a preceding action";
        let hash = qdoc::blobstore::put(bytes);
        let resources = provide_resources(&ctx, &state.doc);
        let descriptor = qos_actions::door::survey(&resources)
            .into_iter()
            .find(|descriptor| descriptor.address.action == "place-image")
            .expect("the document publishes its content hand-off action");
        assert!(descriptor.input.wants_content());

        let arguments = serde_json::Map::from_iter([(
            qos_actions::PROMPTED_PARAM.to_string(),
            serde_json::Value::String(hash.clone()),
        )]);
        qos_actions::door::invoke_json(
            &mut state.doc,
            &resources,
            &descriptor.address,
            &arguments,
            b"agent",
            &registry(),
            &mut LocalTransport::new(),
        )
        .expect("the app resolves and places available content");

        assert!(body_blocks(&state.doc).iter().any(|block| {
            block.kind == "image"
                && block.attrs.get("blob") == Some(&qdoc::AttrVal::Str(hash.clone()))
        }));
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
        let after_img = qdoc::model::blocks_in_order(
            &state.doc,
            &qdoc::model::open(&state.doc, &automerge::ROOT).unwrap(),
        );
        let last = after_img.last().cloned().unwrap();
        dispatch(&mut state.doc, &insert_table_action(&last, 2, 2));

        let ctx = WpResourceCtx {
            caret_block: para.clone(),
            caret_offset: 0,
            target_blocks: vec![para],
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
        };
        let resources = provide_resources(&ctx, &state.doc);
        assert_eq!(resources.len(), 1, "one document root");
        let root = &resources[0];
        let ids_of = |kind: &str| root.children.iter().filter(|r| r.kind == kind).count();
        assert_eq!(ids_of("word_processor.image"), 1, "an image resource");
        assert_eq!(ids_of("word_processor.table"), 1, "a table resource");
        assert!(
            ids_of("word_processor.section") >= 1,
            "a heading section resource"
        );

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("Insert table");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Insert table")),
            "{labels:?}"
        );
        palette.set_query("Delete image");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete image")),
            "{labels:?}"
        );
        palette.set_query("Delete table");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete table")),
            "{labels:?}"
        );
    }

    #[test]
    fn review_entities_are_resources_and_decisions_replay_identically() {
        let (mut a, ids) = seeded("Review this sentence\n");
        let block = ids[0].clone();
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let mut id_gen = qdoc::IdGen::new(a.doc.get_actor().to_hex_string());
        let (comment, change) = {
            let mut dm = qdoc::DocMut {
                doc: &mut a.doc,
                c: &c,
                ids: &mut id_gen,
            };
            (
                dm.add_comment(&block, 0, 6, "Maya", "Please clarify this."),
                dm.track_insert(&block, 20, " now", "Jonas", "2026-09-05"),
            )
        };
        a.doc.commit();
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();

        let ctx = WpResourceCtx {
            caret_block: block,
            caret_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: RegionKind::Body,
        };
        let resources = provide_resources(&ctx, &a.doc);
        let review = resources[0]
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "word-processor.review")
            .expect("review group is navigable");
        let comment_resource_id = format!("word-processor.comment:{comment}");
        let comment_resource = review
            .children
            .iter()
            .find(|resource| resource.id.as_str() == comment_resource_id.as_str())
            .expect("comment is a resource");
        assert!(
            comment_resource
                .actions
                .iter()
                .any(|action| action.id == "resolve")
        );
        let change_resource_id = format!("word-processor.change:{change}");
        let change_resource = review
            .children
            .iter()
            .find(|resource| resource.id.as_str() == change_resource_id.as_str())
            .expect("tracked change is a resource");
        let accept = immediate_encode(
            change_resource
                .actions
                .iter()
                .find(|action| action.id == "accept")
                .unwrap(),
        );
        let action: WordProcessorAction = serde_json::from_slice(&accept.payload).unwrap();
        let envelope = qos_actions::apply_local(&mut a.doc, &action, accept).unwrap();
        qos_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(a.doc.get_heads(), b.doc.get_heads());
        let snap = qdoc::model::materialize(
            &a.doc,
            &qdoc::model::open(&a.doc, &automerge::ROOT).unwrap(),
        );
        assert!(snap.changes.is_empty(), "accepted change left review state");
        assert_eq!(snap.comments.len(), 1, "unrelated comment remains");
    }

    #[test]
    fn stale_multi_target_action_is_an_atomic_noop() {
        let (mut state, ids) = seeded("first\nsecond\n");
        let before = state.doc.save();
        let action = set_block_attr_action(
            RegionKind::Body,
            vec![ids[0].clone(), "deleted-by-a-peer".to_string()],
            "align",
            AttrVal::Str("center".to_string()),
        );
        let result = action.apply(&mut state.doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(
            state.doc.save(),
            before,
            "one stale target must not leave the valid target partly changed"
        );
    }

    #[test]
    fn hostile_table_dimensions_are_rejected_before_any_write() {
        let (mut state, ids) = seeded("anchor\n");
        let before = state.doc.save();
        let result = insert_table_action(&ids[0], MAX_TABLE_DIMENSION + 1, 1).apply(&mut state.doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(state.doc.save(), before);
    }

    #[test]
    fn unbound_document_is_refused_without_a_write() {
        let mut doc = AutoCommit::new();
        let before = doc.save();
        let result = WordProcessorAction::ReplaceBody {
            markdown: "# should not initialize an arbitrary document".to_string(),
        }
        .apply(&mut doc);
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn cross_block_selection_replacement_is_one_replayable_action() {
        let (mut a, ids) = seeded("alpha\n\nbeta\n\ngamma\n");
        let bytes = a.save();
        let mut b = ui::State::load(&bytes).unwrap();
        let action = replace_selection_action(RegionKind::Body, &ids[0], 2, &ids[2], 3, "X");
        let encoded = encode(&action);
        let envelope = qos_actions::apply_local(&mut a.doc, &action, encoded).unwrap();
        qos_actions::replay(&mut b.doc, &envelope, &registry()).unwrap();
        assert_eq!(a.doc.get_heads(), b.doc.get_heads());
        let c = qdoc::model::open(&a.doc, &automerge::ROOT).unwrap();
        let blocks = qdoc::model::materialize(&a.doc, &c).blocks;
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "alXma");
    }
}
