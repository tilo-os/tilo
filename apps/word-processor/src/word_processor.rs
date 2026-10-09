//! `WordProcessor` — a rich-text editor over a `qdoc` document living in a host
//! `AutoCommit` (e.g. `ui::State.doc`). This module holds the **editing model**:
//! a caret `(block_id, char_offset)` and the keyboard edit operations
//! (insert / Enter-split / Backspace-merge / caret motion) expressed as
//! mutations of the canonical document. Rendering + the `ui::Widget` impl live
//! in `widget.rs`; click-to-caret needs layout and is wired there.

use crate::num;
use crate::styles::{StyleProps, StyleSheet};
use automerge::{AutoCommit, ObjId, ReadDoc};
use tilo_doc::model::Region;
use tilo_doc::{Containers, IdGen};
use ui::text_edit::{Edit, EditCore};
#[cfg(test)]
use ui::widget::Edits;

/// One block's `(block_id, start, end)` character range, as returned by the
/// selection/format-span readers below.
pub type BlockSpans = Vec<(String, usize, usize)>;

/// Caret position: a block id plus a character offset within that block's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub block: String,
    pub offset: usize,
}

/// Order a caret/anchor pair in document order using block indices.
///
/// Shared by the editing model (CRDT block order) and the page widget (its
/// cached `BlockView` list), so draw-time and mutation-time selection cannot
/// disagree on the comparison rule.
pub(crate) fn ordered_selection_span(
    anchor: Option<&Caret>,
    caret: &Caret,
    block_index: impl Fn(&str) -> Option<usize>,
) -> Option<(Caret, Caret)> {
    let anchor = anchor?;
    if anchor == caret {
        return None;
    }
    match (block_index(&anchor.block), block_index(&caret.block)) {
        (Some(pa), Some(pc)) => {
            if (pa, anchor.offset) <= (pc, caret.offset) {
                Some((anchor.clone(), caret.clone()))
            } else {
                Some((caret.clone(), anchor.clone()))
            }
        }
        _ => None,
    }
}

/// Per-block `(id, start, end)` char ranges spanning an ordered selection.
pub(crate) fn selection_block_ranges(
    start: &Caret,
    end: &Caret,
    block_index: impl Fn(&str) -> Option<usize>,
    block_at: impl Fn(usize) -> Option<(String, usize)>,
) -> Vec<(String, usize, usize)> {
    let (Some(si), Some(ei)) = (block_index(&start.block), block_index(&end.block)) else {
        return vec![];
    };
    (si..=ei)
        .filter_map(|i| {
            let (id, blen) = block_at(i)?;
            let s = if i == si { start.offset } else { 0 };
            let e = if i == ei { end.offset } else { blen };
            Some((id, s, e))
        })
        .collect()
}

pub use tilo_document::ClipBlock;

/// One entry of the document outline / navigation pane: a heading block's id,
/// its `level` (1-based; defaults to 1), and its current text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub id: String,
    pub level: i64,
    pub text: String,
}

/// True when a materialized block was generated as part of the table of contents
/// (carries a truthy `toc` attr). Used to find the old TOC for replacement and to
/// keep TOC lines out of the heading outline.
pub(crate) fn block_is_toc(b: &tilo_doc::snapshot::BlockView) -> bool {
    matches!(b.attrs.get("toc"), Some(tilo_doc::AttrVal::Bool(true)))
}

/// Parse the comma-separated `tab_stops` attr into px positions, sorted ascending
/// with near-duplicates collapsed. Blank / malformed entries are skipped.
pub(crate) fn parse_tab_stops(raw: &str) -> Vec<f64> {
    let mut v: Vec<f64> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        // `parse` accepts "nan"/"inf" as valid floats; a non-finite stop would
        // draw off-screen forever and corrupt ruler-position math, so reject it
        // like any other malformed entry.
        .filter(|p| p.is_finite() && *p >= 0.0)
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    v
}

/// The editing core. `bind` resolves/initialises the document; the edit ops
/// mutate the host `AutoCommit` and keep the caret consistent.
pub struct WordProcessor {
    pub(crate) containers: Option<Containers>,
    pub(crate) ids: Option<IdGen>,
    pub(crate) caret: Caret,
    /// Selection anchor; a selection exists when this is set and differs from
    /// the caret (within the same block, for v1).
    pub(crate) selection_anchor: Option<Caret>,
    /// The active **edit context** — which page region the caret + edit ops
    /// address. `Body` is the default canonical flow; `Header`/`Footer` route
    /// every text/structure op to the page header / footer container instead (so
    /// clicking into a running-head band lets the user type there). Per-region
    /// caret state is stashed in `region_carets` so switching contexts (and back)
    /// restores where the caret was.
    pub(crate) region: Region,
    /// Stashed `(caret, anchor)` for the regions not currently active, so e.g.
    /// returning to the body re-seats the caret where it was. Indexed by region.
    body_caret: Option<(Caret, Option<Caret>)>,
    header_caret: Option<(Caret, Option<Caret>)>,
    footer_caret: Option<(Caret, Option<Caret>)>,
    /// Shared byte-safe horizontal edit core for the active block. The
    /// surrounding model still owns block splits/merges and qdoc structure.
    edit_core: EditCore,
    /// Where the document's binary inputs (its images) live. The document only references them
    /// by content address. In memory until the app hands over its store.
    content: tilo_content::ContentStore,
    /// Where every edit goes: the framework's dispatcher, which checks the
    /// participant's permission, attributes the edit and groups typing into
    /// undo steps. Without one (a view that was never attached) nothing is
    /// written.
    dispatch: Option<ui::framework::Dispatcher<crate::framework::Document>>,
}

impl Default for WordProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessor {
    pub fn new() -> Self {
        Self {
            containers: None,
            ids: None,
            caret: Caret::default(),
            selection_anchor: None,
            region: Region::Body,
            body_caret: None,
            header_caret: None,
            footer_caret: None,
            edit_core: EditCore::new(),
            content: tilo_content::ContentStore::memory(),
            dispatch: None,
        }
    }

    /// Submit every edit through `dispatch` from now on.
    pub(crate) fn attach(
        &mut self,
        dispatch: ui::framework::Dispatcher<crate::framework::Document>,
    ) {
        self.dispatch = Some(dispatch);
    }

    /// Keep the document's binary inputs in `content` (the app's store) from now on.
    pub fn set_content_store(&mut self, content: tilo_content::ContentStore) {
        self.content = content;
    }

    /// The store holding the document's binary inputs.
    pub fn content_store(&self) -> &tilo_content::ContentStore {
        &self.content
    }

    // ---- edit context (Body / Header / Footer) -----------------------------

    /// The region the caret + edit ops currently address.
    pub fn region(&self) -> Region {
        self.region
    }

    /// True when editing the page header or footer (not the body flow).
    pub fn in_running_region(&self) -> bool {
        !matches!(self.region, Region::Body)
    }

    fn caret_slot(&mut self, region: Region) -> &mut Option<(Caret, Option<Caret>)> {
        match region {
            Region::Body => &mut self.body_caret,
            Region::Header => &mut self.header_caret,
            Region::Footer => &mut self.footer_caret,
        }
    }

    /// Switch the active edit context to `region`, stashing the current caret +
    /// selection under the old region and restoring (or seeding) the new one.
    /// For Header/Footer this lazily creates an empty paragraph if the region is
    /// empty, so there is always a block to type into. Returns the caret block id
    /// the context lands on (empty string only if the doc isn't bound).
    pub fn focus_region(&mut self, state: &mut ui::widget::Edits<'_>, region: Region) -> String {
        if region == self.region {
            // Already here — still guarantee a block exists (header/footer may be
            // empty) and the caret is valid.
            return self.ensure_region_block(state);
        }
        // Stash the outgoing region's caret/selection.
        let prev = (self.caret.clone(), self.selection_anchor.clone());
        let prev_region = self.region;
        *self.caret_slot(prev_region) = Some(prev);

        self.region = region;
        // Restore the incoming region's caret if we have one and it still exists;
        // otherwise fall back to its first block.
        let restored = self.caret_slot(region).take();
        if let Some((c, anchor)) = restored {
            self.caret = c;
            self.selection_anchor = anchor;
        } else {
            self.caret = Caret::default();
            self.selection_anchor = None;
        }
        self.ensure_region_block(state)
    }

    /// Convenience: focus the body context (Esc / clicking the body).
    pub fn focus_body(&mut self, state: &mut ui::widget::Edits<'_>) -> String {
        self.focus_region(state, Region::Body)
    }

    /// Guarantee the active region has at least one block and the caret sits on a
    /// real one (clamped). Creates an empty paragraph in an empty header/footer.
    /// Returns the resolved caret block id.
    fn ensure_region_block(&mut self, state: &mut ui::widget::Edits<'_>) -> String {
        if self.containers.is_none() {
            return String::new();
        }
        let blocks = self.region_blocks_in_order(state);
        if blocks.is_empty() {
            let action = crate::actions::RichTextAction::EnsureRegionBlock {
                region: self.action_region(),
            };
            if !self.apply_rich(state, action) {
                return String::new();
            }
            let Some(id) = self.region_blocks_in_order(state).into_iter().next() else {
                return String::new();
            };
            self.caret = Caret {
                block: id.clone(),
                offset: 0,
            };
            self.selection_anchor = None;
            return id;
        }
        if blocks.contains(&self.caret.block) {
            let len = self.region_block_len(state, &self.caret.block);
            if self.caret.offset > len {
                self.caret.offset = len;
            }
        } else if let Some(first) = blocks.first() {
            self.caret = Caret {
                block: first.clone(),
                offset: 0,
            };
            self.selection_anchor = None;
        }
        self.caret.block.clone()
    }

    // ---- selection ---------------------------------------------------------

    /// Begin (or keep) a selection anchored at the current caret.
    pub fn set_anchor(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.caret.clone());
        }
    }

    pub fn clear_anchor(&mut self) {
        self.selection_anchor = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection_anchor
            .as_ref()
            .is_some_and(|a| a != &self.caret)
    }

    /// Selection endpoints in document order, if non-empty.
    pub fn selection_span(&self, state: &AutoCommit) -> Option<(Caret, Caret)> {
        // Bail before `ordered_blocks`: a lone caret must not scan the body
        // list (see `framework_host` keystroke-cost test).
        let anchor = self.selection_anchor.as_ref()?;
        if anchor == &self.caret {
            return None;
        }
        let blocks = self.ordered_blocks(state);
        ordered_selection_span(Some(anchor), &self.caret, |id| {
            blocks.iter().position(|b| b == id)
        })
    }

    /// Per-block selected ranges `(block, start, end)` spanning the selection.
    pub fn selection_blocks(&self, state: &AutoCommit) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span(state) else {
            return vec![];
        };
        let blocks = self.ordered_blocks(state);
        selection_block_ranges(
            &start,
            &end,
            |id| blocks.iter().position(|b| b == id),
            |i| {
                let b = blocks.get(i)?;
                Some((b.clone(), self.block_len(state, b)))
            },
        )
    }

    /// The selected text, joining blocks with newlines.
    pub fn selection_text(&self, state: &AutoCommit) -> String {
        let Some(containers) = self.containers.as_ref() else {
            return String::new();
        };
        self.selection_blocks(state)
            .into_iter()
            .map(|(b, s, e)| {
                tilo_doc::model::block_text(state, containers, &b)
                    .unwrap_or_default()
                    .chars()
                    .skip(s)
                    .take(e.saturating_sub(s))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Build a rich fragment of the selection: `(plain_text, blocks)`. The plain
    /// text doubles as a clipboard key so paste knows the system clipboard still
    /// holds this fragment.
    pub fn copy_fragment(&self, state: &AutoCommit) -> (String, Vec<ClipBlock>) {
        let Some(containers) = self.containers.as_ref() else {
            return (String::new(), vec![]);
        };
        let snap = tilo_doc::model::materialize(state, containers);
        let mut blocks = Vec::new();
        let mut plain = Vec::new();
        for (bid, s, e) in self.selection_blocks(state) {
            let Some(b) = snap.blocks.iter().find(|b| b.id == bid) else {
                continue;
            };
            let chars: Vec<char> = b.text.chars().collect();
            let (s, e) = (s.min(chars.len()), e.min(chars.len()));
            let text: String = chars
                .get(s..e)
                .map_or_else(String::new, |c| c.iter().collect());
            plain.push(text.clone());
            let marks = b
                .marks
                .iter()
                .filter_map(|m| {
                    let (ms, me) = (m.start.max(s), m.end.min(e));
                    (ms < me).then(|| tilo_doc::document::MarkSpan {
                        start: ms - s,
                        end: me - s,
                        name: m.name.clone(),
                        value: m.value.clone(),
                    })
                })
                .collect();
            let int = |k: &str| match b.attrs.get(k) {
                Some(tilo_doc::AttrVal::Int(n)) => Some(*n),
                _ => None,
            };
            let bool_ = |k: &str| match b.attrs.get(k) {
                Some(tilo_doc::AttrVal::Bool(v)) => Some(*v),
                _ => None,
            };
            let str_ = |k: &str| match b.attrs.get(k) {
                Some(tilo_doc::AttrVal::Str(v)) => Some(v.clone()),
                _ => None,
            };
            blocks.push(ClipBlock {
                kind: b.kind.clone(),
                text,
                marks,
                level: int("level"),
                ordered: bool_("ordered"),
                align: str_("align"),
                attrs: std::collections::BTreeMap::default(),
            });
        }
        (plain.join("\n"), blocks)
    }

    /// Replace the character range `[char_start, char_end)` of `block_id` with
    /// `text` (used by Find & Replace).
    pub fn splice_block(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        block_id: &str,
        char_start: usize,
        char_end: usize,
        text: &str,
    ) {
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SpliceText {
            region: self.action_region(),
            block: block_id.to_string(),
            at: char_start,
            del: char_end.saturating_sub(char_start),
            ins: text.to_string(),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Replace the **entire body** with `markdown`, in place, on the live document:
    /// existing block ids are deleted and imported content appended as new blocks,
    /// so undo/redo and sync see one coherent action. Used by "New from template"
    /// (re-import over the open document) without discarding the host `ui::State`'s
    /// undo history / collaboration session. Caret moves to the first imported
    /// block; returns `false` (document untouched) if unbound or import fails.
    pub fn replace_with_markdown(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        markdown: &[u8],
    ) -> bool {
        self.replace_with_format(state, markdown, crate::WpFormat::Markdown)
    }

    /// Replace the whole body with `bytes` decoded as `format` — the in-place
    /// **Open** path, mirroring [`Self::replace_with_markdown`] but through the
    /// format-aware codec. A decode failure leaves the document and caret intact.
    pub fn replace_with_format(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        bytes: &[u8],
        format: crate::WpFormat,
    ) -> bool {
        let Some(action) = self.stage_replacement(bytes, format) else {
            return false;
        };
        if !self.apply_rich(state, action) {
            return false;
        }
        self.seat_after_replacement(state);
        true
    }

    /// Check that `bytes` import as `format`, and keep any images they name.
    /// Does not write `state`. The returned action is what Open submits.
    pub fn stage_replacement(
        &mut self,
        bytes: &[u8],
        format: crate::WpFormat,
    ) -> Option<crate::actions::RichTextAction> {
        crate::import_action(bytes, format, &self.content)
            .map_err(|error| eprintln!("word-processor: could not open the file: {error}"))
            .ok()
    }

    /// Move the caret to the start of the document after a replacement action
    /// has been applied.
    pub fn seat_after_replacement(&mut self, state: &AutoCommit) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        self.ids = Some(tilo_doc::model::seeded_id_gen(state, &containers));
        let blocks = tilo_doc::model::blocks_in_order(state, &containers);
        match blocks.first() {
            Some(first) => {
                self.caret = Caret {
                    block: first.clone(),
                    offset: 0,
                }
            }
            None => self.caret = Caret::default(),
        }
        self.clear_anchor();
    }

    /// Load the document's stylesheet from the **CRDT styles container** (the
    /// field-level-mergeable, ODF-round-trippable source of truth), or the
    /// built-in defaults when it holds none.
    pub fn stylesheet(&self, state: &AutoCommit) -> StyleSheet {
        let Some(c) = self.containers.as_ref() else {
            return StyleSheet::default();
        };
        let ids = tilo_doc::model::style_ids(state, c);
        if ids.is_empty() {
            return StyleSheet::default();
        }
        // Built-in defaults are the base layer so plain blocks always resolve to a
        // real style (an imported `.odt` only writes the *named* styles its blocks
        // reference into the container, not the built-ins); container styles —
        // imported, user-defined, or edited built-ins — overlay them.
        let mut sheet = StyleSheet::default();
        for id in ids {
            // Typed style actions persist individual fields. Missing fields
            // keep the resolved base style instead of becoming zero metrics.
            let mut props = sheet.get(&id).to_attrs();
            props.extend(tilo_doc::model::style_props(state, c, &id));
            sheet.styles.insert(id, StyleProps::from_attrs(&props));
        }
        sheet
    }

    /// Persist the stylesheet into the **CRDT styles container** — one property
    /// per [`crate::actions::RichTextAction::SetStyleProperty`], so replicas
    /// merge fields. A custom style dropped from `sheet` is deleted. Built-in
    /// styles stay; they are the default layer under the container.
    pub fn set_stylesheet(&mut self, state: &mut ui::widget::Edits<'_>, sheet: &StyleSheet) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        if self.ids.is_none() {
            return;
        }
        let existing = tilo_doc::model::style_ids(state, &containers);
        for (id, props) in &sheet.styles {
            for (key, value) in props.to_attrs() {
                let value = match value {
                    tilo_doc::AttrVal::Bool(v) => crate::actions::AttrVal::Bool(v),
                    tilo_doc::AttrVal::Int(v) => crate::actions::AttrVal::Int(v),
                    tilo_doc::AttrVal::Float(v) => crate::actions::AttrVal::Float(v),
                    tilo_doc::AttrVal::Str(v) => crate::actions::AttrVal::Str(v),
                };
                let action = crate::actions::RichTextAction::SetStyleProperty {
                    style: id.clone(),
                    key,
                    value,
                };
                let _ = self.apply_rich(state, action);
            }
        }
        let snap = tilo_doc::model::materialize(state, &containers);
        for id in existing {
            if sheet.styles.contains_key(&id) || StyleSheet::is_builtin(&id) {
                continue;
            }
            let fallback_blocks = snap
                .blocks
                .iter()
                .filter(|block| {
                    matches!(block.attrs.get("style"), Some(tilo_doc::AttrVal::Str(style)) if style == &id)
                })
                .map(|block| block.id.clone())
                .collect();
            let action = crate::actions::RichTextAction::DeleteStyle {
                style: id,
                fallback_blocks,
            };
            let _ = self.apply_rich(state, action);
        }
    }

    /// The document's persisted page margins as `[top, right, bottom, left]` (px),
    /// or `None` when the document does not record them (a fresh / legacy doc, so
    /// the widget keeps its defaults). Stored as four document-level meta strings
    /// so page setup survives save/reopen — part of the document, not the widget.
    pub fn page_margins(&self, state: &AutoCommit) -> Option<[f64; 4]> {
        let c = self.containers.as_ref()?;
        let get =
            |k: &str| tilo_doc::model::meta_str(state, c, k).and_then(|s| s.parse::<f64>().ok());
        Some([
            get("page_margin_top")?,
            get("page_margin_right")?,
            get("page_margin_bottom")?,
            get("page_margin_left")?,
        ])
    }

    /// Persist the page margins `[top, right, bottom, left]` (px) into the document
    /// as four meta registers, so they round-trip through save/reopen and merge
    /// field-by-field across replicas.
    pub fn set_page_margins(&mut self, state: &mut ui::widget::Edits<'_>, margins: [f64; 4]) {
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetPageMargins {
            top: margins[0],
            right: margins[1],
            bottom: margins[2],
            left: margins[3],
        };
        let _ = self.apply_rich(state, action);
    }

    /// Assign a style id to every block in the selection (or the caret's block).
    /// Empty clears it, reverting to the kind-derived default style.
    pub fn set_block_style(&mut self, state: &mut ui::widget::Edits<'_>, style_id: &str) {
        if self.containers.is_none() {
            return;
        }
        let targets = self
            .selection_blocks(state)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>();
        let targets = if targets.is_empty() {
            vec![self.caret.block.clone()]
        } else {
            targets
        };
        if targets.is_empty() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetBlockAttr {
            region: self.action_region(),
            blocks: targets,
            key: "style".into(),
            value: crate::actions::AttrVal::Str(style_id.to_string()),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Delete a **custom** paragraph style: remove it from the document's style
    /// container and clear the `style` attr of every block that referenced it (so
    /// those blocks fall back to their kind-derived default). Built-in styles are
    /// never deletable — the call is a no-op and returns `false`. Returns `true`
    /// when the style existed and was removed.
    pub fn delete_style(&mut self, state: &mut ui::widget::Edits<'_>, style_id: &str) -> bool {
        if StyleSheet::is_builtin(style_id) {
            return false;
        }
        let Some(containers) = self.containers.clone() else {
            return false;
        };
        if !tilo_doc::model::style_ids(state, &containers)
            .iter()
            .any(|s| s == style_id)
        {
            return false;
        }
        // Blocks that explicitly reference the doomed style must lose the ref so
        // they render against the built-in default instead of a dangling id.
        let orphans: Vec<String> = tilo_doc::model::materialize(state, &containers)
            .blocks
            .iter()
            .filter(
                |b| matches!(b.attrs.get("style"), Some(tilo_doc::AttrVal::Str(s)) if s == style_id),
            )
            .map(|b| b.id.clone())
            .collect();
        let action = crate::actions::RichTextAction::DeleteStyle {
            style: style_id.to_string(),
            fallback_blocks: orphans,
        };
        self.apply_rich(state, action)
    }

    /// Duplicate the paragraph style `style_id` under a fresh `customN` id,
    /// naming it "`<name>` copy", and persist it into the document's style
    /// container. Returns the new style id (or `None` if the source is unknown).
    /// The caller decides whether to also apply it to the caret block.
    pub fn duplicate_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        style_id: &str,
    ) -> Option<String> {
        let mut sheet = self.stylesheet(state);
        let mut props = sheet.styles.get(style_id)?.clone();
        let new_id = sheet.fresh_custom_id();
        props.name = format!("{} copy", props.name);
        sheet.styles.insert(new_id.clone(), props);
        self.set_stylesheet(state, &sheet);
        Some(new_id)
    }

    /// The caret's block id when it is an `image` block (i.e. an image is
    /// "selected"), else `None`. Drives the panel's image-inspector affordances
    /// (resize / flip) and the on-page selection handles.
    pub fn selected_image_block(&self, state: &AutoCommit) -> Option<String> {
        let c = self.containers.as_ref()?;
        let snap = tilo_doc::model::materialize(state, c);
        snap.blocks
            .iter()
            .find(|b| b.id == self.caret.block && b.kind == "image")
            .map(|b| b.id.clone())
    }

    /// Set the explicit display size (px) of an `image` block, recording `width`
    /// and `height` attrs the renderer/exporter honour. Values are floored to a
    /// sane minimum so an image can never be scaled to nothing.
    pub fn set_image_size(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        block_id: &str,
        width: f64,
        height: f64,
    ) {
        if self.containers.is_none() {
            return;
        }
        let w = num::i64_of_f64(width.round().clamp(16.0, 4000.0));
        let h = num::i64_of_f64(height.round().clamp(16.0, 4000.0));
        if self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetImageSize {
            block: block_id.to_string(),
            width: w,
            height: h,
        };
        let _ = self.apply_rich(state, action);
    }

    /// Toggle a mirror flip on an `image` block: `horizontal` flips the
    /// `flip_h` attr (mirror left↔right), otherwise the `flip_v` attr (mirror
    /// top↔bottom). Returns the new flip state for that axis.
    pub fn flip_image(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        block_id: &str,
        horizontal: bool,
    ) -> bool {
        let Some(containers) = self.containers.clone() else {
            return false;
        };
        let key = if horizontal { "flip_h" } else { "flip_v" };
        let cur = tilo_doc::model::materialize(state, &containers)
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .is_some_and(|b| matches!(b.attrs.get(key), Some(tilo_doc::AttrVal::Bool(true))));
        let next = !cur;
        if self.ids.is_none() {
            return next;
        }
        let action = crate::actions::RichTextAction::SetBlockAttr {
            region: self.action_region(),
            blocks: vec![block_id.to_string()],
            key: key.into(),
            value: crate::actions::AttrVal::Bool(next),
        };
        let _ = self.apply_rich(state, action);
        next
    }

    /// The explicit `(width, height)` display size of an `image` block, if both
    /// attrs are set. Lets the panel seed its resize field from the live doc.
    pub fn image_size(&self, state: &AutoCommit, block_id: &str) -> Option<(f64, f64)> {
        let c = self.containers.as_ref()?;
        let b = tilo_doc::model::materialize(state, c)
            .blocks
            .into_iter()
            .find(|b| b.id == block_id)?;
        let num = |k: &str| match b.attrs.get(k) {
            Some(tilo_doc::AttrVal::Int(n)) if *n > 0 => Some(num::f64_of_i64(*n)),
            Some(tilo_doc::AttrVal::Float(f)) if *f > 0.0 => Some(*f),
            _ => None,
        };
        Some((num("width")?, num("height")?))
    }

    /// Insert a rich fragment at the caret (replacing the selection), preserving
    /// marks + paragraph styles.
    pub fn insert_rich(&mut self, state: &mut ui::widget::Edits<'_>, blocks: &[ClipBlock]) {
        if blocks.is_empty() {
            self.delete_selection(state);
            return;
        }
        let (start, end) = self.selection_span(state).unwrap_or_else(|| {
            let caret = self.caret.clone();
            (caret.clone(), caret)
        });
        let len = self.block_len(state, &start.block);
        let was_empty = if start.block == end.block {
            start.offset == 0 && end.offset >= len
        } else {
            start.offset == 0
        };
        let adopt_first = was_empty
            && blocks
                .first()
                .is_some_and(|b| b.kind != "cell" && b.kind != "fidelity");
        let action = crate::actions::paste_fragment_action(
            self.action_region(),
            crate::actions::SelectionTarget {
                start_block: start.block.clone(),
                start: start.offset,
                end_block: end.block,
                end: end.offset,
            },
            blocks.to_vec(),
            adopt_first,
        );
        if !self.apply_rich(state, action) {
            return;
        }
        let order = self.ordered_blocks(state);
        let Some(pos) = order.iter().position(|id| id == &start.block) else {
            return;
        };
        let last = pos + blocks.len() - 1;
        let Some(block) = order.get(last) else {
            return;
        };
        let added = blocks.last().map_or(0, |block| block.text.chars().count());
        self.caret = Caret {
            block: block.clone(),
            offset: if blocks.len() == 1 {
                start.offset + added
            } else {
                added
            },
        };
        self.selection_anchor = None;
    }

    fn selected_block_ids(&self, state: &AutoCommit) -> Vec<String> {
        let sel = self.selection_blocks(state);
        if sel.is_empty() {
            vec![self.caret.block.clone()]
        } else {
            sel.into_iter().map(|(b, _, _)| b).collect()
        }
    }

    /// Delete the current selection (no-op if none); returns whether it deleted.
    pub fn delete_selection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let Some((start, end)) = self.selection_span(state) else {
            return false;
        };
        let action = crate::actions::RichTextAction::ReplaceSelection {
            region: self.action_region(),
            start_block: start.block.clone(),
            start: start.offset,
            end_block: end.block,
            end: end.offset,
            ins: String::new(),
        };
        if !self.apply_rich(state, action) {
            return false;
        }
        self.caret = start;
        self.selection_anchor = None;
        true
    }

    /// Set paragraph style on every selected block (or the caret block). Table
    /// cells are skipped — turning a cell into a heading/list would break the
    /// table (alignment and font still apply to cells via their own methods).
    pub fn set_paragraph_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        kind: &str,
        level: Option<i64>,
        ordered: Option<bool>,
    ) {
        let Some(containers) = self.containers.clone() else {
            return;
        };
        let snap = tilo_doc::model::materialize(state, &containers);
        let targets: Vec<String> = self
            .selected_block_ids(state)
            .into_iter()
            .filter(|id| {
                snap.blocks
                    .iter()
                    .find(|b| &b.id == id)
                    .is_none_or(|b| b.kind != "cell")
            })
            .collect();
        if targets.is_empty() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetParagraphStyle {
            region: self.action_region(),
            blocks: targets,
            kind: kind.to_string(),
            level,
            ordered,
        };
        let _ = self.apply_rich(state, action);
    }

    fn set_block_attr(
        &self,
        state: &mut ui::widget::Edits<'_>,
        blocks: Vec<String>,
        key: &str,
        value: crate::actions::AttrVal,
    ) {
        if self.containers.is_none() || self.ids.is_none() || blocks.is_empty() {
            return;
        }
        let action = crate::actions::RichTextAction::SetBlockAttr {
            region: self.action_region(),
            blocks,
            key: key.to_string(),
            value,
        };
        let _ = self.apply_rich(state, action);
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers construct owned RichTextAction values at the call site"
    )]
    fn apply_rich(
        &self,
        state: &mut ui::widget::Edits<'_>,
        action: crate::actions::RichTextAction,
    ) -> bool {
        if self.containers.is_none() || self.ids.is_none() {
            return false;
        }
        let Some(dispatch) = &self.dispatch else {
            return false;
        };
        let context = tilo_actions::ActionContext::for_app("org.tilo.Writer", "Writer");
        dispatch
            .apply_now_with(state, &crate::model::action(&action), context)
            .is_ok()
    }

    fn action_region(&self) -> crate::actions::RegionKind {
        match self.region {
            Region::Body => crate::actions::RegionKind::Body,
            Region::Header => crate::actions::RegionKind::Header,
            Region::Footer => crate::actions::RegionKind::Footer,
        }
    }

    fn mark_spans(ranges: &[(String, usize, usize)]) -> Vec<crate::actions::Span> {
        ranges
            .iter()
            .filter(|(_, start, end)| start < end)
            .map(|(block, start, end)| crate::actions::Span {
                block: block.clone(),
                start: *start,
                end: *end,
            })
            .collect()
    }

    fn mark_val(value: &tilo_doc::document::MarkValue) -> crate::actions::MarkVal {
        match value {
            tilo_doc::document::MarkValue::Bool(flag) => crate::actions::MarkVal::Bool(*flag),
            tilo_doc::document::MarkValue::Int(n) => crate::actions::MarkVal::Int(*n),
            tilo_doc::document::MarkValue::Str(text) => crate::actions::MarkVal::Str(text.clone()),
        }
    }

    /// Set alignment on every block in the selection (or the caret block).
    pub fn set_align(&mut self, state: &mut ui::widget::Edits<'_>, align: &str) {
        if self.containers.is_none() {
            return;
        }
        let targets = self.selected_block_ids(state);
        if targets.is_empty() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetBlockAttr {
            region: self.action_region(),
            blocks: targets,
            key: "align".into(),
            value: crate::actions::AttrVal::Str(align.to_string()),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Whether the caret's block forces a page break before it (the
    /// `page_break_before` block attr). False for a fresh/legacy block.
    pub fn caret_page_break_before(&self, state: &AutoCommit) -> bool {
        let Some(c) = self.containers.as_ref() else {
            return false;
        };
        let snap = tilo_doc::model::materialize(state, c);
        snap.blocks
            .iter()
            .find(|b| b.id == self.caret.block)
            .is_some_and(|b| {
                matches!(
                    b.attrs.get("page_break_before"),
                    Some(tilo_doc::AttrVal::Bool(true))
                )
            })
    }

    /// Set the `page_break_before` attr on the caret's block (forcing pagination
    /// to start that block on a fresh page when `on`). Table cells are skipped —
    /// a cell breaking mid-table would split the grid.
    pub fn set_page_break_before(&mut self, state: &mut ui::widget::Edits<'_>, on: bool) {
        if self.containers.is_none() {
            return;
        }
        let block = self.caret.block.clone();
        if self.block_meta(state, &block).0 == "cell" || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetBlockAttr {
            region: self.action_region(),
            blocks: vec![block],
            key: "page_break_before".into(),
            value: crate::actions::AttrVal::Bool(on),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Toggle the page break on the caret's block (Ctrl+Enter when the caret
    /// already starts a paragraph). Returns the new state.
    pub fn toggle_page_break_before(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let on = !self.caret_page_break_before(state);
        self.set_page_break_before(state, on);
        on
    }

    /// Insert a manual page break at the caret: split the current block (like
    /// Enter) and mark the new block to start on a fresh page. The caret follows
    /// into the new block so typing continues on the next page.
    pub fn insert_page_break(&mut self, state: &mut ui::widget::Edits<'_>) {
        // A page break inside a table cell would split the grid; ignore it.
        if self.block_meta(state, &self.caret.block).0 == "cell" {
            return;
        }
        self.enter(state);
        self.set_page_break_before(state, true);
    }

    /// Char ranges to apply an inline font mark to: the selection, or — with no
    /// selection — the whole caret block.
    /// selection — the whole caret block.
    fn font_ranges(&self, state: &AutoCommit) -> Vec<(String, usize, usize)> {
        if self.has_selection() {
            self.selection_blocks(state)
        } else {
            let len = self.block_len(state, &self.caret.block);
            vec![(self.caret.block.clone(), 0, len)]
        }
    }

    /// Set a **non-toggle** inline mark to a specific `value` over the selection
    /// (per-run), or — with no selection — over the whole caret block. Unlike
    /// `toggle_mark` (which flips a boolean on/off), this *replaces* any prior
    /// value of the same-named mark over each range: it removes the existing mark
    /// then re-adds it at `value`, so changing e.g. a run's size/colour/font
    /// overwrites rather than stacking. Used for the valued character formats
    /// (`color`, `font`, `size`).
    pub fn set_mark_value(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        value: &tilo_doc::document::MarkValue,
    ) {
        let ranges = self.font_ranges(state);
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let spans = Self::mark_spans(&ranges);
        if spans.is_empty() {
            return;
        }
        let action = crate::actions::RichTextAction::SetMark {
            region: self.action_region(),
            spans,
            name: name.to_string(),
            value: Some(Self::mark_val(value)),
            also_clear: Vec::new(),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Set the font size (pt) over the selection (per-run), or the caret block.
    /// Stored as a `size` Int run mark (the codec contract's character format).
    pub fn set_font_size(&mut self, state: &mut ui::widget::Edits<'_>, size: i64) {
        self.set_mark_value(state, "size", &tilo_doc::document::MarkValue::Int(size));
    }

    /// Set the font family (`sans` | `serif` | `mono`) over the selection
    /// (per-run), or the caret block. Stored as a `font` Str run mark.
    pub fn set_font_family(&mut self, state: &mut ui::widget::Edits<'_>, family: &str) {
        self.set_mark_value(
            state,
            "font",
            &tilo_doc::document::MarkValue::Str(family.to_string()),
        );
    }

    /// Set the text colour (`#rrggbb`) over the selection (per-run), or the caret
    /// block. Stored as a `color` Str run mark.
    pub fn set_text_color(&mut self, state: &mut ui::widget::Edits<'_>, hex: &str) {
        self.set_mark_value(
            state,
            "color",
            &tilo_doc::document::MarkValue::Str(hex.to_string()),
        );
    }

    /// Remove a named run mark over the selection (per-run), or the caret block —
    /// the inverse of `set_mark_value`. Used by the toolbar's neutral colour
    /// swatch to clear a `color` mark (reverting to the block's style ink).
    pub fn clear_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        let ranges = self.font_ranges(state);
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let spans = Self::mark_spans(&ranges);
        if spans.is_empty() {
            return;
        }
        let action = crate::actions::RichTextAction::SetMark {
            region: self.action_region(),
            spans,
            name: name.to_string(),
            value: None,
            also_clear: Vec::new(),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Toggle an inline mark over the selection (across blocks). Region-aware.
    pub fn toggle_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        let sel = self.selection_blocks(state);
        if sel.is_empty() || self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let all_marked = sel
            .iter()
            .filter(|(_, s, e)| s < e)
            .all(|(b, s, e)| self.region_has_mark_over(state, b, *s, *e, name));
        let spans = Self::mark_spans(&sel);
        if spans.is_empty() {
            return;
        }
        let action = crate::actions::RichTextAction::SetMark {
            region: self.action_region(),
            spans,
            name: name.to_string(),
            value: if all_marked {
                None
            } else {
                Some(crate::actions::MarkVal::Bool(true))
            },
            also_clear: Vec::new(),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Set the highlight (background) colour (`#rrggbb`) over the selection
    /// (per-run), or the caret block. Stored as a `highlight` Str run mark; the
    /// widget maps it to a `RichSpan.background` band behind the glyphs.
    pub fn set_highlight(&mut self, state: &mut ui::widget::Edits<'_>, hex: &str) {
        self.set_mark_value(
            state,
            "highlight",
            &tilo_doc::document::MarkValue::Str(hex.to_string()),
        );
    }

    // ---- tracked changes / comments review --------------------------------

    /// Accept a tracked change by id (insertion: keep the text, drop its `ins`
    /// mark; deletion: remove the struck text). The change record is removed from
    /// the `changes` container, so it stops being reported in the materialized
    /// snapshot. No-op for an unknown id or an unbound document.
    pub fn accept_change(&mut self, state: &mut ui::widget::Edits<'_>, change_id: &str) {
        self.review_change(state, Some(change_id), true);
    }

    /// Reject a tracked change by id (insertion: remove the inserted text;
    /// deletion: keep the text, drop its `del` mark) — the inverse of
    /// [`Self::accept_change`]. The change record is removed either way. No-op
    /// for an unknown id or an unbound document.
    pub fn reject_change(&mut self, state: &mut ui::widget::Edits<'_>, change_id: &str) {
        self.review_change(state, Some(change_id), false);
    }

    /// Accept every tracked change in the document. Processed in reverse
    /// document order so an accepted deletion's text removal never shifts the
    /// anchored range of an earlier-but-not-yet-processed change.
    pub fn accept_all_changes(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.review_change(state, None, true);
    }

    /// Reject every tracked change in the document (see [`Self::accept_all_changes`]
    /// for the ordering rationale).
    pub fn reject_all_changes(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.review_change(state, None, false);
    }

    fn review_change(&self, state: &mut ui::widget::Edits<'_>, id: Option<&str>, accept: bool) {
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = if accept {
            crate::actions::RichTextAction::AcceptChanges {
                id: id.map(str::to_string),
            }
        } else {
            crate::actions::RichTextAction::RejectChanges {
                id: id.map(str::to_string),
            }
        };
        let _ = self.apply_rich(state, action);
    }

    /// The tracked-change ids present in the document, sorted by their anchored
    /// `(block-order, start)` body position (earliest first). Falls back to the
    /// canonical id order for changes whose anchoring mark is gone.
    #[allow(
        dead_code,
        reason = "review navigation helper kept beside Accept/Reject until wired"
    )]
    fn changes_in_doc_order(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return Vec::new();
        };
        let block_order = self.ordered_blocks(state);
        let pos = |bid: &str| {
            block_order
                .iter()
                .position(|b| b == bid)
                .unwrap_or(usize::MAX)
        };
        let mut ids = tilo_doc::model::change_ids(state, c);
        ids.sort_by_key(|id| {
            tilo_doc::model::change_range(state, c, id)
                .map_or((usize::MAX, usize::MAX), |(b, s, _)| (pos(&b), s))
        });
        ids
    }

    /// Whether a named boolean run mark covers the whole of `ranges`.
    fn mark_over_ranges(
        &self,
        state: &AutoCommit,
        ranges: &[(String, usize, usize)],
        name: &str,
    ) -> bool {
        let Some(containers) = self.containers.as_ref() else {
            return false;
        };
        let active: Vec<_> = ranges.iter().filter(|(_, s, e)| s < e).collect();
        if active.is_empty() {
            return false;
        }
        active
            .iter()
            .all(|(b, s, e)| tilo_doc::model::has_mark_over(state, containers, b, *s, *e, name))
    }

    // ---- action planning (pure reads shared with the action model) ----------
    //
    // These expose the selection/mark resolution the private edit ops use, so
    // the widget can build the matching `RichTextAction` and dispatch it
    // through the Tilo executor instead of mutating the document directly. They
    // never mutate — the action's `apply` performs the write.

    /// The active [`RegionKind`](crate::actions::RegionKind) the caret + edit ops
    /// address (body / header / footer).
    pub fn region_kind(&self) -> crate::actions::RegionKind {
        crate::actions::RegionKind::from_region(self.region)
    }

    /// The blocks a block-level format targets: the selection, or the caret
    /// block when there is no selection.
    pub fn target_block_ids(&self, state: &AutoCommit) -> Vec<String> {
        self.selected_block_ids(state)
    }

    /// The blocks a paragraph-style change targets — the selection (or caret
    /// block), minus table cells (a cell keeps its `cell` kind).
    pub fn paragraph_style_targets(&self, state: &AutoCommit) -> Vec<String> {
        let Some(containers) = self.containers.as_ref() else {
            return Vec::new();
        };
        let snap = tilo_doc::model::materialize(state, containers);
        self.selected_block_ids(state)
            .into_iter()
            .filter(|id| {
                snap.blocks
                    .iter()
                    .find(|b| &b.id == id)
                    .is_none_or(|b| b.kind != "cell")
            })
            .collect()
    }

    /// The character ranges a valued/exclusive inline mark covers: the selection
    /// per-run, or — with no selection — the whole caret block. Empty runs are
    /// kept for the caller to skip.
    pub fn format_spans(&self, state: &AutoCommit) -> BlockSpans {
        self.font_ranges(state)
    }

    /// Plan a **toggle** inline mark (bold/italic/…): the selection spans and
    /// whether the mark is already active over them (so the caller removes
    /// rather than adds). Returns `None` when there is no active selection — a
    /// toggle mark with no selection is a no-op, matching the live editor.
    pub fn plan_toggle_mark(&self, state: &AutoCommit, name: &str) -> Option<(BlockSpans, bool)> {
        let sel = self.selection_blocks(state);
        if self.containers.is_none() || sel.iter().all(|(_, s, e)| s >= e) {
            return None;
        }
        let all_marked = sel
            .iter()
            .filter(|(_, s, e)| s < e)
            .all(|(b, s, e)| self.region_has_mark_over(state, b, *s, *e, name));
        Some((sel, all_marked))
    }

    /// Plan a **mutually-exclusive** toggle mark (super/sub): the ranges (over
    /// the selection or whole caret block) and whether it is already active.
    pub fn plan_exclusive_mark(
        &self,
        state: &AutoCommit,
        name: &str,
    ) -> (Vec<(String, usize, usize)>, bool) {
        let ranges = self.font_ranges(state);
        let on = self.mark_over_ranges(state, &ranges, name);
        (ranges, on)
    }

    /// Toggle a boolean run mark over the selection (per-run), or — with no
    /// selection — the whole caret block. `clears` is a sibling mark removed
    /// whenever this one is turned on, so a pair like `super`/`sub` stays mutually
    /// exclusive. Returns nothing; reads/writes happen in document order.
    fn toggle_exclusive_run_mark(
        &self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        clears: &str,
    ) {
        let ranges = self.font_ranges(state);
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let on = self.mark_over_ranges(state, &ranges, name);
        let spans = Self::mark_spans(&ranges);
        if spans.is_empty() {
            return;
        }
        let action = crate::actions::RichTextAction::SetMark {
            region: self.action_region(),
            spans,
            name: name.to_string(),
            value: if on {
                None
            } else {
                Some(crate::actions::MarkVal::Bool(true))
            },
            also_clear: if on {
                Vec::new()
            } else {
                vec![clears.to_string()]
            },
        };
        let _ = self.apply_rich(state, action);
    }

    /// Toggle the `super` (superscript) run mark over the selection / caret block,
    /// clearing any `sub` mark on the same range (the pair is mutually exclusive).
    pub fn toggle_super(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.toggle_exclusive_run_mark(state, "super", "sub");
    }

    /// Toggle the `sub` (subscript) run mark over the selection / caret block,
    /// clearing any `super` mark on the same range (mutually exclusive).
    pub fn toggle_sub(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.toggle_exclusive_run_mark(state, "sub", "super");
    }

    /// Set the paragraph line-spacing multiplier (1.0 = single) on every block in
    /// the selection (or the caret block). Stored as the `line_spacing` Float
    /// block attr; the layout pass feeds it to `RichTextLayout::shape` as the
    /// `line_height_mult`. A multiplier of `1.0` clears the attr (the default).
    pub fn set_line_spacing(&mut self, state: &mut ui::widget::Edits<'_>, mult: f64) {
        self.set_block_attr(
            state,
            self.selected_block_ids(state),
            "line_spacing",
            crate::actions::AttrVal::Float(mult),
        );
    }

    /// The line-spacing multiplier on the caret's block (the `line_spacing` Float
    /// attr), or `1.0` when unset. Drives the toolbar's cycling control.
    pub fn caret_line_spacing(&self, state: &AutoCommit) -> f64 {
        let Some(c) = self.containers.as_ref() else {
            return 1.0;
        };
        let snap = tilo_doc::model::materialize(state, c);
        snap.blocks
            .iter()
            .find(|b| b.id == self.caret.block)
            .map_or(1.0, |b| match b.attrs.get("line_spacing") {
                // A non-finite or non-positive value (a malformed import, or a
                // remote peer's action) is not a usable multiplier — fall back
                // like an unset attr rather than propagating it.
                Some(tilo_doc::AttrVal::Float(f)) if f.is_finite() && *f > 0.0 => *f,
                Some(tilo_doc::AttrVal::Int(n)) if *n > 0 => num::f64_of_i64(*n),
                _ => 1.0,
            })
    }

    // ---- ruler: per-block indents + tab stops --------------------------------
    // The horizontal ruler edits three per-paragraph block attrs (so they merge
    // field-by-field and survive save/reload): `indent` (left/hanging indent px,
    // added on top of the style's indent), `first_line_indent` (extra indent on
    // the paragraph's first line only — a v1 layout limitation), and `tab_stops`
    // (a comma-separated px-position list, since qdoc's attr map stores only
    // scalars).

    /// Read a numeric attr (`Float`/`Int`) off an arbitrary block by id, `0.0`
    /// when absent. Shared by the indent getters below.
    fn block_num_attr(&self, state: &AutoCommit, block: &str, key: &str) -> f64 {
        let Some(c) = self.containers.as_ref() else {
            return 0.0;
        };
        let snap = tilo_doc::model::materialize(state, c);
        snap.blocks
            .iter()
            .find(|b| b.id == block)
            .map_or(0.0, |b| match b.attrs.get(key) {
                Some(tilo_doc::AttrVal::Float(f)) => *f,
                Some(tilo_doc::AttrVal::Int(n)) => num::f64_of_i64(*n),
                _ => 0.0,
            })
    }

    /// The left / hanging indent (px) of an arbitrary block — the `indent` attr,
    /// `0.0` when unset.
    pub fn block_indent(&self, state: &AutoCommit, block: &str) -> f64 {
        self.block_num_attr(state, block, "indent")
    }

    /// The first-line indent (px) of an arbitrary block — the `first_line_indent`
    /// attr, `0.0` when unset.
    pub fn block_first_line_indent(&self, state: &AutoCommit, block: &str) -> f64 {
        self.block_num_attr(state, block, "first_line_indent")
    }

    /// Set the left / hanging indent (px) on `block`. Clamped non-negative.
    pub fn set_block_indent(&mut self, state: &mut ui::widget::Edits<'_>, block: &str, px: f64) {
        self.set_block_attr(
            state,
            vec![block.to_string()],
            "indent",
            crate::actions::AttrVal::Float(px.max(0.0)),
        );
    }

    /// Set the first-line indent (px) on `block`. May be negative (a hanging
    /// indent draws the first line back toward the margin).
    pub fn set_block_first_line_indent(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        block: &str,
        px: f64,
    ) {
        self.set_block_attr(
            state,
            vec![block.to_string()],
            "first_line_indent",
            crate::actions::AttrVal::Float(px),
        );
    }

    /// The tab stops (px positions from the left text margin) of an arbitrary
    /// block, sorted ascending. Empty when the `tab_stops` attr is unset/blank.
    pub fn block_tab_stops(&self, state: &AutoCommit, block: &str) -> Vec<f64> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        let snap = tilo_doc::model::materialize(state, c);
        let raw = snap
            .blocks
            .iter()
            .find(|b| b.id == block)
            .and_then(|b| match b.attrs.get("tab_stops") {
                Some(tilo_doc::AttrVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        parse_tab_stops(&raw)
    }

    /// Tab stops on the caret's block (convenience for the widget).
    pub fn caret_tab_stops(&self, state: &AutoCommit) -> Vec<f64> {
        self.block_tab_stops(state, &self.caret.block.clone())
    }

    /// Replace `block`'s tab stops with `stops` (deduplicated + sorted). An empty
    /// list clears the attr to the empty string.
    pub fn set_block_tab_stops(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        block: &str,
        stops: &[f64],
    ) {
        if self.containers.is_none() {
            return;
        }
        let mut v: Vec<f64> = stops.iter().copied().filter(|p| *p >= 0.0).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        let encoded = v
            .iter()
            .map(|p| format!("{p:.1}"))
            .collect::<Vec<_>>()
            .join(",");
        self.set_block_attr(
            state,
            vec![block.to_string()],
            "tab_stops",
            crate::actions::AttrVal::Str(encoded),
        );
    }

    /// Add a tab stop at `px` (from the left text margin) on the caret's block.
    pub fn add_tab_stop(&mut self, state: &mut ui::widget::Edits<'_>, px: f64) {
        let block = self.caret.block.clone();
        let mut stops = self.block_tab_stops(state, &block);
        // Ignore a stop that coincides with an existing one.
        if stops.iter().any(|p| (*p - px).abs() < 2.0) {
            return;
        }
        stops.push(px.max(0.0));
        self.set_block_tab_stops(state, &block, &stops);
    }

    /// Tab key: advance the caret to the next tab stop on its block, if any.
    /// Returns `true` when a stop existed and the caret advanced (the caller then
    /// skips the legacy "insert spaces" fallback). The advance is recorded in the
    /// document as a single tab character so it survives save/reload and reflows
    /// with edits — the layout treats the run up to the next stop as the tab's
    /// width (full per-stop glyph expansion is a v1 limitation).
    pub fn tab_advance(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let stops = self.block_tab_stops(state, &self.caret.block.clone());
        if stops.is_empty() {
            return false;
        }
        self.insert_str(state, "\t");
        true
    }

    /// Insert a `rows`×`cols` table after the caret block. Cells are modeled as
    /// consecutive top-level `cell` blocks tagged with `table`/`row`/`col`, so
    /// they edit/select/collaborate through the normal block machinery; the
    /// widget renders consecutive same-`table` cells as a grid.
    pub fn insert_table(&mut self, state: &mut ui::widget::Edits<'_>, rows: usize, cols: usize) {
        let after = self.caret.block.clone();
        let action = crate::actions::insert_table_action(&after, rows, cols);
        if !self.apply_rich(state, action) {
            return;
        }
        let blocks = self.ordered_blocks(state);
        let Some(pos) = blocks.iter().position(|block| block == &after) else {
            return;
        };
        let Some(first) = blocks.get(pos + 1) else {
            return;
        };
        self.caret = Caret {
            block: first.clone(),
            offset: 0,
        };
        self.selection_anchor = None;
    }

    /// Insert an image block carrying `bytes` at the caret; the bytes go in the
    /// app's content store and the block's `blob` attr references them
    /// (mirroring `tilo_doc_codecs::odt`). Mid-text, the block splits so the image
    /// lands between the text before/after the caret; else it inserts after the
    /// caret block. The caret lands in a fresh empty paragraph just after the image.
    ///
    /// Decodable bytes record intrinsic pixel `width`/`height` as attrs (matching
    /// ODF import); undecodable bytes still produce a valid block (a placeholder).
    pub fn insert_image_bytes(&mut self, state: &mut ui::widget::Edits<'_>, bytes: &[u8]) {
        if self.containers.is_none() {
            return;
        }
        // Store before changing the selection/text.  A quota rejection must be
        // a no-op, not a split paragraph with a dangling content reference.
        let hash = match self.content.put(bytes) {
            Ok(hash) => hash,
            Err(error) => {
                eprintln!("word-processor: image not kept, nothing inserted: {error}");
                return;
            }
        };
        self.delete_selection(state);
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        let action = crate::actions::insert_image_action(
            &block,
            offset,
            crate::actions::ImageInput::of(hash, bytes),
        );
        if !self.apply_rich(state, action) {
            return;
        }
        // The action splits the caret block, inserts the image, then a paragraph
        // that carries any tail. The caret continues in that paragraph.
        let order = self.ordered_blocks(state);
        if let Some(pos) = order.iter().position(|id| id == &block)
            && let Some(after) = order.get(pos + 2)
        {
            self.caret = Caret {
                block: after.clone(),
                offset: 0,
            };
        }
        self.selection_anchor = None;
    }

    /// `(table id, Vec<(cell_id, row, col)>)` for the table the caret is in.
    fn caret_table(&self, state: &AutoCommit) -> Option<(String, BlockSpans)> {
        let containers = self.containers.as_ref()?;
        let snap = tilo_doc::model::materialize(state, containers);
        let cur = snap.blocks.iter().find(|b| b.id == self.caret.block)?;
        let tid = match cur.attrs.get("table") {
            Some(tilo_doc::AttrVal::Str(s)) => s.clone(),
            _ => return None,
        };
        let int = |b: &tilo_doc::snapshot::BlockView, k: &str| match b.attrs.get(k) {
            Some(tilo_doc::AttrVal::Int(n)) if *n >= 0 => num::usize_of_i64(*n),
            _ => 0,
        };
        let cells = snap
            .blocks
            .iter()
            .filter(|b| {
                b.kind == "cell"
                    && matches!(b.attrs.get("table"), Some(tilo_doc::AttrVal::Str(s)) if s == &tid)
            })
            .map(|b| (b.id.clone(), int(b, "row"), int(b, "col")))
            .collect();
        Some((tid, cells))
    }

    fn caret_rc(&self, cells: &[(String, usize, usize)]) -> (usize, usize) {
        cells
            .iter()
            .find(|(id, _, _)| id == &self.caret.block)
            .map_or((0, 0), |(_, r, c)| (*r, *c))
    }

    /// Add a row below the caret's row (toolbar `+Row`).
    pub fn table_add_row(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_row(state, true);
    }

    /// Insert a blank row immediately above the caret's row.
    pub fn insert_row_above(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_row(state, false);
    }

    /// Insert a blank row immediately below the caret's row.
    pub fn insert_row_below(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_row(state, true);
    }

    /// Insert a full-width blank row adjacent to the caret's row. `below` selects
    /// the side (`true` → below the caret row, `false` → above it). The table is
    /// kept rectangular: the new row has exactly `ncols` cells and every row at or
    /// past the insertion point has its `row` attr bumped by one. The caret moves
    /// to the first cell of the new row.
    fn insert_row(&mut self, state: &mut ui::widget::Edits<'_>, below: bool) {
        let Some((tid, cells)) = self.caret_table(state) else {
            return;
        };
        let (cur_row, _) = self.caret_rc(&cells);
        let new_row = if below { cur_row + 1 } else { cur_row };
        let action = crate::actions::edit_table_action(
            &tid,
            crate::actions::TableEdit::InsertRow { at: new_row },
        );
        if !self.apply_rich(state, action) {
            return;
        }
        let Some((_, cells)) = self.caret_table(state) else {
            return;
        };
        let Some((id, _, _)) = cells
            .iter()
            .find(|(_, row, col)| *row == new_row && *col == 0)
        else {
            return;
        };
        self.caret = Caret {
            block: id.clone(),
            offset: 0,
        };
        self.selection_anchor = None;
    }

    /// Add a column to the right of the caret's column (toolbar `+Col`).
    pub fn table_add_col(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_col(state, true);
    }

    /// Insert a blank column immediately to the left of the caret's column.
    pub fn insert_col_left(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_col(state, false);
    }

    /// Insert a blank column immediately to the right of the caret's column.
    pub fn insert_col_right(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.insert_col(state, true);
    }

    /// Insert a blank column adjacent to the caret's column. `right` selects the
    /// side (`true` → right of the caret column, `false` → left). The table stays
    /// rectangular: one cell is added per row at `new_col`, and every existing
    /// cell at or past `new_col` has its `col` attr bumped. The caret moves to the
    /// new cell in the caret's row.
    fn insert_col(&mut self, state: &mut ui::widget::Edits<'_>, right: bool) {
        let Some((tid, cells)) = self.caret_table(state) else {
            return;
        };
        let (cur_row, cur_col) = self.caret_rc(&cells);
        let new_col = if right { cur_col + 1 } else { cur_col };
        let action = crate::actions::edit_table_action(
            &tid,
            crate::actions::TableEdit::InsertColumn { at: new_col },
        );
        if !self.apply_rich(state, action) {
            return;
        }
        let Some((_, cells)) = self.caret_table(state) else {
            return;
        };
        let Some((id, _, _)) = cells
            .iter()
            .find(|(_, row, col)| *row == cur_row && *col == new_col)
        else {
            return;
        };
        self.caret = Caret {
            block: id.clone(),
            offset: 0,
        };
        self.selection_anchor = None;
    }

    /// `true` when the caret currently sits inside a table cell.
    pub fn caret_in_cell(&self, state: &AutoCommit) -> bool {
        self.caret_table(state).is_some()
    }

    /// Tab / Shift-Tab cell navigation. `forward` moves to the next cell in
    /// row-major order (wrapping to the first cell of the next row). At the
    /// table's **last** cell it stays put; the widget publishes a typed absolute
    /// row-insertion action before calling this navigation helper. `!forward`
    /// moves to the previous cell
    /// (staying put at the very first cell). Returns `true` when the caret was in
    /// a cell and was handled here (so the caller should not insert a tab).
    pub fn tab_cell(&mut self, state: &mut ui::widget::Edits<'_>, forward: bool) -> bool {
        let Some((_, cells)) = self.caret_table(state) else {
            return false;
        };
        // Cells in row-major order (row, then col) — the navigation order.
        let mut ordered: Vec<(String, usize, usize)> = cells;
        ordered.sort_by_key(|(_, r, c)| (*r, *c));
        let Some(pos) = ordered
            .iter()
            .position(|(id, _, _)| id == &self.caret.block)
        else {
            return false;
        };
        if forward {
            if let Some((next, ..)) = ordered.get(pos + 1).cloned() {
                self.caret = Caret {
                    block: next,
                    offset: 0,
                };
                self.selection_anchor = None;
            }
        } else if let Some((prev, ..)) = pos.checked_sub(1).and_then(|i| ordered.get(i).cloned()) {
            // Land at the end of the previous cell's text (natural Shift-Tab feel).
            let off = self.block_len(state, &prev);
            self.caret = Caret {
                block: prev,
                offset: off,
            };
            self.selection_anchor = None;
        }
        true
    }

    /// Vertical cell navigation (Up/Down) when the caret is at the top/bottom text
    /// edge of a cell: move to the vertically-adjacent cell (same column, row
    /// ±1). Returns `true` when it moved between cells; `false` lets the caller
    /// fall back to ordinary block-relative motion.
    pub fn cell_vertical(&mut self, state: &AutoCommit, down: bool) -> bool {
        let Some((_, cells)) = self.caret_table(state) else {
            return false;
        };
        let (cur_row, cur_col) = self.caret_rc(&cells);
        let target_row = if down {
            cur_row + 1
        } else {
            cur_row.checked_sub(1).unwrap_or(usize::MAX)
        };
        if target_row == usize::MAX {
            return false;
        }
        // Prefer the cell directly above/below; if that column is absent in the
        // target row (ragged tables shouldn't happen, but be safe), pick the
        // nearest column there.
        let in_row: Vec<&(String, usize, usize)> =
            cells.iter().filter(|(_, r, _)| *r == target_row).collect();
        if in_row.is_empty() {
            return false;
        }
        let pick = in_row
            .iter()
            .min_by_key(|(_, _, c)| c.abs_diff(cur_col))
            .map(|(id, _, _)| id.clone());
        if let Some(id) = pick {
            let off = self.caret.offset.min(self.block_len(state, &id));
            self.caret = Caret {
                block: id,
                offset: off,
            };
            self.selection_anchor = None;
            return true;
        }
        false
    }

    /// Delete the caret's row (deletes the whole table if it was the last row).
    pub fn table_del_row(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some((tid, cells)) = self.caret_table(state) else {
            return;
        };
        let nrows = cells.iter().map(|(_, r, _)| *r).max().unwrap_or(0) + 1;
        if nrows <= 1 {
            self.table_delete(state);
            return;
        }
        let (cur_row, cur_col) = self.caret_rc(&cells);
        let target_row = if cur_row + 1 < nrows {
            cur_row + 1
        } else {
            cur_row - 1
        };
        let new_caret = cells
            .iter()
            .find(|(_, r, c)| *r == target_row && *c == cur_col)
            .map(|(id, _, _)| id.clone());
        let action = crate::actions::edit_table_action(
            &tid,
            crate::actions::TableEdit::DeleteRow { row: cur_row },
        );
        if !self.apply_rich(state, action) {
            return;
        }
        if let Some(nc) = new_caret {
            self.caret = Caret {
                block: nc,
                offset: 0,
            };
            self.selection_anchor = None;
        }
    }

    /// Delete the caret's column (deletes the whole table if it was the last).
    pub fn table_del_col(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some((tid, cells)) = self.caret_table(state) else {
            return;
        };
        let ncols = cells.iter().map(|(_, _, c)| *c).max().unwrap_or(0) + 1;
        if ncols <= 1 {
            self.table_delete(state);
            return;
        }
        let (cur_row, cur_col) = self.caret_rc(&cells);
        let target_col = if cur_col + 1 < ncols {
            cur_col + 1
        } else {
            cur_col - 1
        };
        let new_caret = cells
            .iter()
            .find(|(_, r, c)| *r == cur_row && *c == target_col)
            .map(|(id, _, _)| id.clone());
        let action = crate::actions::edit_table_action(
            &tid,
            crate::actions::TableEdit::DeleteColumn { column: cur_col },
        );
        if !self.apply_rich(state, action) {
            return;
        }
        if let Some(nc) = new_caret {
            self.caret = Caret {
                block: nc,
                offset: 0,
            };
            self.selection_anchor = None;
        }
    }

    /// Delete the whole table the caret is in; caret moves to the block before it.
    pub fn table_delete(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some((tid, cells)) = self.caret_table(state) else {
            return;
        };
        let order = self.ordered_blocks(state);
        let first_idx = cells
            .iter()
            .filter_map(|(id, _, _)| order.iter().position(|b| b == id))
            .min();
        let before = first_idx.and_then(|i| {
            if i > 0 {
                order.get(i - 1).cloned()
            } else {
                None
            }
        });
        let action = crate::actions::edit_table_action(&tid, crate::actions::TableEdit::Delete);
        if !self.apply_rich(state, action) {
            return;
        }
        let remaining = self.ordered_blocks(state);
        if let Some(t) = before.or_else(|| remaining.first().cloned()) {
            let off = self.block_len(state, &t);
            self.caret = Caret {
                block: t,
                offset: off,
            };
        } else {
            let action = crate::actions::RichTextAction::EnsureRegionBlock {
                region: crate::actions::RegionKind::Body,
            };
            if self.apply_rich(state, action)
                && let Some(id) = self.ordered_blocks(state).into_iter().next()
            {
                self.caret = Caret {
                    block: id,
                    offset: 0,
                };
            }
        }
        self.selection_anchor = None;
    }

    /// Insert text, splitting on newlines into new paragraphs (multi-line paste).
    pub fn insert_multiline(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        let normalized = text.replace('\r', "");
        let mut first = true;
        for line in normalized.split('\n') {
            if !first {
                self.enter(state);
            }
            if !line.is_empty() {
                self.insert_str(state, line);
            }
            first = false;
        }
    }

    /// Reconcile caret state without authoring document changes.
    pub fn reconcile(&mut self, state: &AutoCommit) {
        // The caret's block has to exist in the *active region's* blocks, not
        // the body's — otherwise a caret seated in a header or footer reads as
        // stray on every body-only check and gets dragged back to the body.
        let mut blocks = self.region_blocks_in_order(state);
        if blocks.is_empty() {
            // The running head being edited has lost its last block, so there
            // is nothing there to type into and the body is the context again.
            self.region = Region::Body;
            blocks = self.region_blocks_in_order(state);
        }
        if blocks.contains(&self.caret.block) {
            // clamp offset to the (possibly remotely-shrunk) block length
            let len = self.block_len(state, &self.caret.block);
            if self.caret.offset > len {
                self.caret.offset = len;
            }
        } else if let Some(first) = blocks.first().cloned() {
            // Nothing seated yet, or the block the caret was on is gone. The
            // first position of this region is where to be, and an anchor
            // into a vanished block must not survive it.
            self.caret = Caret {
                block: first,
                offset: 0,
            };
            self.selection_anchor = None;
        }
    }

    /// Resolve the document model under `root` (initialising the schema if
    /// absent), guarantee at least one block, and keep the caret on a real block.
    /// Bind to a document this participant may only view: resolve it and
    /// seat the caret, writing nothing.
    pub fn bind_view_only(&mut self, state: &AutoCommit, root: &ObjId) {
        if self.containers.is_none() {
            let Some(c) = crate::model::body_at(state, root)
                .and_then(|body| tilo_doc::model::open(state, &body).ok())
            else {
                return;
            };
            self.containers = Some(c);
        }
        let Some(c) = self.containers.as_ref() else {
            return;
        };
        let seated = matches!(
            state.get(&c.blocks, &self.caret.block),
            Ok(Some((automerge::Value::Object(automerge::ObjType::Map), _)))
        );
        if !seated {
            self.region = Region::Body;
            self.caret = tilo_doc::model::blocks_in_order(state, c)
                .into_iter()
                .next()
                .map(|block| Caret { block, offset: 0 })
                .unwrap_or_default();
            self.selection_anchor = None;
        }
    }

    pub fn bind(&mut self, state: &AutoCommit, root: &ObjId) {
        // A canonical body caret proves that the body is nonempty. Resolve
        // only its block instead of sorting the entire body twice per sync.
        // Match ordered_keys membership exactly; a map key alone is not enough.
        if self.region == Region::Body
            && self.containers.as_ref().is_some_and(|c| {
                tilo_doc::model::region_block_len(state, c, Region::Body, &self.caret.block)
                    .is_some()
            })
        {
            self.caret.offset = self
                .caret
                .offset
                .min(self.block_len(state, &self.caret.block));
            return;
        }
        if self.containers.is_none() {
            // The record's genesis made the document; a view never creates it.
            let Some(c) = crate::model::body_at(state, root)
                .and_then(|body| tilo_doc::model::open(state, &body).ok())
            else {
                return;
            };
            // Seed past existing block ids so editing a loaded/imported document
            // never re-mints (and overwrites) an existing block.
            self.ids = Some(tilo_doc::model::seeded_id_gen(state, &c));
            self.containers = Some(c);
        }
        self.reconcile(state);
    }

    // ---- region-aware primitives -------------------------------------------
    // The caret/edit machinery below addresses whichever region is active
    // (`self.region`), dispatching each op to the body or region-aware (`*_in`)
    // qdoc ops, so `insert_str`/`enter`/`backspace`/motion/marks are written once
    // and work in body, header, and footer alike.

    /// Block ids of the **active region**, in order.
    fn region_blocks_in_order(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        tilo_doc::model::region_blocks_in_order(state, c, self.region)
    }

    /// Text of a block in the **active region** (body uses the body reader;
    /// header/footer scan the materialized region — the body-only `block_text`
    /// reader hard-codes the body container).
    fn region_block_text(&self, state: &AutoCommit, block: &str) -> Option<String> {
        let c = self.containers.as_ref()?;
        match self.region {
            Region::Body => tilo_doc::model::block_text(state, c, block),
            r => tilo_doc::model::materialize_region(state, c, r)
                .into_iter()
                .find(|b| b.id == block)
                .map(|b| b.text),
        }
    }

    fn region_block_len(&self, state: &AutoCommit, block: &str) -> usize {
        self.region_block_text(state, block)
            .map_or(0, |t| t.chars().count())
    }

    /// Whether a boolean mark covers `[start,end)` of a block in the active region.
    fn region_has_mark_over(
        &self,
        state: &AutoCommit,
        block: &str,
        start: usize,
        end: usize,
        name: &str,
    ) -> bool {
        let Some(c) = self.containers.as_ref() else {
            return false;
        };
        match self.region {
            Region::Body => tilo_doc::model::has_mark_over(state, c, block, start, end, name),
            r => tilo_doc::model::materialize_region(state, c, r)
                .into_iter()
                .find(|b| b.id == block)
                .is_some_and(|b| {
                    b.marks
                        .iter()
                        .any(|m| m.name == name && m.start <= start && m.end >= end)
                }),
        }
    }

    // ---- region-aware writes (dispatch body vs. header/footer) -------------
    // Each goes through the region-aware qdoc op (`*_in`) for the active region.

    fn region_splice(
        &self,
        state: &mut ui::widget::Edits<'_>,
        block: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) {
        let Ok(del) = usize::try_from(del) else {
            return;
        };
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SpliceText {
            region: self.action_region(),
            block: block.to_string(),
            at: idx,
            del,
            ins: ins.to_string(),
        };
        let _ = self.apply_rich(state, action);
    }

    fn region_set_kind(&self, state: &mut ui::widget::Edits<'_>, block: &str, kind: &str) {
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = crate::actions::RichTextAction::SetParagraphStyle {
            region: self.action_region(),
            blocks: vec![block.to_string()],
            kind: kind.to_string(),
            level: None,
            ordered: None,
        };
        let _ = self.apply_rich(state, action);
    }

    fn region_remove_block(&self, state: &mut ui::widget::Edits<'_>, block: &str) {
        if self.containers.is_none() || self.ids.is_none() {
            return;
        }
        let action = if self.region == Region::Body {
            crate::actions::RichTextAction::RemoveBlocks {
                blocks: vec![block.to_string()],
            }
        } else {
            crate::actions::RichTextAction::RemoveRegionBlocks {
                region: self.action_region(),
                blocks: vec![block.to_string()],
            }
        };
        let _ = self.apply_rich(state, action);
    }

    #[allow(
        dead_code,
        reason = "clear-mark helper kept for region edits not yet on RichTextAction"
    )]
    fn region_remove_mark(
        &self,
        state: &mut ui::widget::Edits<'_>,
        block: &str,
        start: usize,
        end: usize,
        name: &str,
    ) {
        if start >= end {
            return;
        }
        let action = crate::actions::RichTextAction::SetMark {
            region: self.action_region(),
            spans: vec![crate::actions::Span {
                block: block.to_string(),
                start,
                end,
            }],
            name: name.to_string(),
            value: None,
            also_clear: Vec::new(),
        };
        let _ = self.apply_rich(state, action);
    }

    /// Insert a page-number field at the caret in the active region. Works in the
    /// body too (renders as a `field` mark; the body renderer substitutes it).
    pub fn insert_page_number(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.delete_selection(state);
        let block = self.caret.block.clone();
        let idx = self.caret.offset;
        let action = crate::actions::insert_page_number_action(self.action_region(), &block, idx);
        if !self.apply_rich(state, action) {
            return;
        }
        // The placeholder occupies one character.
        self.caret.offset = idx + 1;
        self.clear_anchor();
    }

    // ---- read helpers ------------------------------------------------------

    fn block_len(&self, state: &AutoCommit, block: &str) -> usize {
        self.region_block_len(state, block)
    }

    /// Text of the block the caret is in (in the active region).
    pub fn current_text(&self, state: &AutoCommit) -> String {
        self.region_block_text(state, &self.caret.block)
            .unwrap_or_default()
    }

    /// Ordered block texts (empty blocks render as empty strings).
    pub fn block_texts(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        tilo_doc::model::blocks_in_order(state, c)
            .iter()
            .map(|id| tilo_doc::model::block_text(state, c, id).unwrap_or_default())
            .collect()
    }

    pub(crate) fn ordered_blocks(&self, state: &AutoCommit) -> Vec<String> {
        self.region_blocks_in_order(state)
    }

    // ---- document outline / navigation ------------------------------------

    /// Every `heading` block in document order, as `(block_id, level, text)`.
    /// `level` defaults to 1 when the `level` attr is absent. Blocks generated
    /// for the table of contents (carrying the `toc` attr) are **excluded** so the
    /// outline and a regenerated TOC never list the TOC's own lines as headings.
    pub fn headings(&self, state: &AutoCommit) -> Vec<Heading> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        let snap = tilo_doc::model::materialize(state, c);
        snap.blocks
            .iter()
            .filter(|b| b.kind == "heading" && !block_is_toc(b))
            .map(|b| {
                let level = match b.attrs.get("level") {
                    Some(tilo_doc::AttrVal::Int(n)) if *n >= 1 => *n,
                    _ => 1,
                };
                Heading {
                    id: b.id.clone(),
                    level,
                    text: b.text.clone(),
                }
            })
            .collect()
    }

    /// Replace any previously generated TOC with a fresh one from `entries`
    /// (each `(level, text, page)`), inserted as `toc`-marked paragraph blocks at
    /// the document top — one per entry. Re-running first removes the old TOC
    /// blocks (by their `toc` attr), so regenerating never duplicates. Each line
    /// is indented by heading level with a dot-leader page number, e.g.
    /// `  Background .......... 3`. Caret stays put unless it pointed at a
    /// removed TOC block, then falls back to the first surviving block. Returns
    /// the inserted TOC block ids (top-to-bottom).
    pub fn generate_toc(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        entries: &[crate::actions::TocTarget],
    ) -> Vec<String> {
        let Some(containers) = self.containers.clone() else {
            return vec![];
        };
        let caret_in_old = tilo_doc::model::materialize(state, &containers)
            .blocks
            .iter()
            .filter(|block| block_is_toc(block))
            .any(|block| block.id == self.caret.block);
        if !self.apply_rich(state, crate::actions::generate_toc_action(entries.to_vec())) {
            return vec![];
        }
        let inserted: Vec<String> = tilo_doc::model::materialize(state, &containers)
            .blocks
            .iter()
            .filter(|block| block_is_toc(block))
            .map(|block| block.id.clone())
            .collect();
        // If the caret sat in a removed TOC block, re-home it so later edits don't
        // target a deleted block.
        if caret_in_old {
            let order = self.ordered_blocks(state);
            if let Some(first) = order.first() {
                self.caret = Caret {
                    block: first.clone(),
                    offset: 0,
                };
            }
        }
        self.selection_anchor = None;
        inserted
    }

    /// `(kind, ordered, level)` for a block in the **active region**.
    fn block_meta(&self, state: &AutoCommit, block: &str) -> (String, bool, Option<i64>) {
        let Some(c) = self.containers.as_ref() else {
            return ("paragraph".into(), false, None);
        };
        let views = match self.region {
            Region::Body => tilo_doc::model::materialize(state, c).blocks,
            r => tilo_doc::model::materialize_region(state, c, r),
        };
        let b = views.iter().find(|b| b.id == block);
        let kind = b.map_or_else(|| "paragraph".into(), |b| b.kind.clone());
        let ordered = b
            .is_some_and(|b| matches!(b.attrs.get("ordered"), Some(tilo_doc::AttrVal::Bool(true))));
        let level = b.and_then(|b| match b.attrs.get("level") {
            Some(tilo_doc::AttrVal::Int(n)) => Some(*n),
            _ => None,
        });
        (kind, ordered, level)
    }

    /// On a space typed right after a markdown-ish line prefix in a plain
    /// paragraph, convert the block's style and consume the prefix. Returns true
    /// if it transformed (caller then skips inserting the space).
    pub fn autoformat_on_space(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let Some(c) = self.containers.clone() else {
            return false;
        };
        let block = self.caret.block.clone();
        if self.block_meta(state, &block).0 != "paragraph" {
            return false;
        }
        let text = tilo_doc::model::block_text(state, &c, &block).unwrap_or_default();
        let prefix: String = text.chars().take(self.caret.offset).collect();
        let (kind, level, ordered): (&str, Option<i64>, Option<bool>) = match prefix.as_str() {
            "-" | "*" | "+" => ("list_item", None, Some(false)),
            "1." | "1)" => ("list_item", None, Some(true)),
            "#" => ("heading", Some(1), None),
            "##" => ("heading", Some(2), None),
            "###" => ("heading", Some(3), None),
            _ => return false,
        };
        let prefix_len = prefix.chars().count();
        let cleared = crate::actions::RichTextAction::SpliceText {
            region: self.action_region(),
            block: block.clone(),
            at: 0,
            del: prefix_len,
            ins: String::new(),
        };
        if !self.apply_rich(state, cleared) {
            return false;
        }
        let styled = crate::actions::RichTextAction::SetParagraphStyle {
            region: self.action_region(),
            blocks: vec![block],
            kind: kind.to_string(),
            level,
            ordered,
        };
        if !self.apply_rich(state, styled) {
            return false;
        }
        self.caret.offset = 0;
        true
    }

    /// Select the whitespace-delimited word under the caret (double-click).
    pub fn select_word(&mut self, state: &AutoCommit) {
        let text = self.current_text(state);
        let byte = text
            .char_indices()
            .nth(self.caret.offset.min(text.chars().count()))
            .map_or(text.len(), |(byte, _)| byte);
        self.edit_core.set_caret(byte);
        self.edit_core.select_word(&text, byte);
        let range = self.edit_core.range();
        let start = range.start;
        let end = range.end;
        let char_at = |byte: usize| {
            text.char_indices()
                .take_while(|(index, _)| *index < byte)
                .count()
        };
        let s = char_at(start);
        let e = char_at(end);
        if s == e {
            return; // caret is on whitespace
        }
        self.caret.offset = s;
        self.clear_anchor();
        self.set_anchor();
        self.caret.offset = e;
    }

    fn sync_edit_core_from_state(&mut self, text: &str) {
        let byte_at = |char_idx: usize| {
            text.char_indices()
                .nth(char_idx)
                .map_or(text.len(), |(byte, _)| byte)
        };
        let anchor = self
            .selection_anchor
            .as_ref()
            .filter(|anchor| anchor.block == self.caret.block)
            .map_or(self.caret.offset, |anchor| anchor.offset);
        self.edit_core
            .set_selection(byte_at(anchor), byte_at(self.caret.offset));
        self.edit_core.clamp_to(text);
    }

    fn sync_state_from_edit_core(&mut self, text: &str) {
        let selection = self.edit_core.selection();
        let char_at = |byte: usize| {
            text.char_indices()
                .take_while(|(index, _)| *index < byte.min(text.len()))
                .count()
        };
        let anchor = char_at(selection.anchor);
        let cursor = char_at(selection.cursor);
        self.caret.offset = cursor;
        self.selection_anchor = (!selection.is_collapsed()).then(|| Caret {
            block: self.caret.block.clone(),
            offset: anchor,
        });
    }

    fn apply_edit_core(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        text: &str,
        edit: &Edit,
    ) -> bool {
        if edit.is_noop() {
            return false;
        }
        let Some(splice) = edit.char_splice(text) else {
            return false;
        };
        let block = self.caret.block.clone();
        self.region_splice(
            state,
            &block,
            splice.at,
            splice.removed_isize(),
            &edit.inserted,
        );
        let mut updated = text.to_owned();
        edit.apply(&mut updated);
        self.sync_state_from_edit_core(&updated);
        true
    }

    /// Delete the character (grapheme cluster) before or after the caret
    /// within its block through the shared core. Returns whether it did.
    fn delete_grapheme(&mut self, state: &mut ui::widget::Edits<'_>, backward: bool) -> bool {
        let text = self.current_text(state);
        self.sync_edit_core_from_state(&text);
        let edit = if backward {
            self.edit_core.delete_backward(&text, false)
        } else {
            self.edit_core.delete_forward(&text, false)
        };
        self.apply_edit_core(state, &text, &edit)
    }

    /// Show `text` as the input method's composition at the caret; it is
    /// not part of the document until committed.
    pub fn set_preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        self.edit_core.set_preedit(text, cursor);
    }

    /// Drop the composition.
    pub fn clear_preedit(&mut self) {
        self.edit_core.clear_preedit();
    }

    /// The composition in progress (empty when none).
    pub fn preedit(&self) -> &str {
        self.edit_core.preedit()
    }

    /// The composition in progress as an edit core over the caret block's
    /// committed `text`, its caret at the document caret, or `None` while
    /// nothing is being composed. Its display mapping lays the composition
    /// out inside the paragraph.
    pub fn composition(&self, text: &str) -> Option<EditCore> {
        if !self.edit_core.has_preedit() {
            return None;
        }
        let mut core = self.edit_core.clone();
        core.set_caret(crate::text_offsets::char_to_byte(text, self.caret.offset));
        core.clamp_to(text);
        Some(core)
    }

    /// Select the entire document (Ctrl+A).
    pub fn select_all(&mut self, state: &AutoCommit) {
        let blocks = self.ordered_blocks(state);
        let (Some(first), Some(last)) = (blocks.first().cloned(), blocks.last().cloned()) else {
            return;
        };
        let last_len = self.block_len(state, &last);
        self.caret = Caret {
            block: first,
            offset: 0,
        };
        self.clear_anchor();
        self.set_anchor();
        self.caret = Caret {
            block: last,
            offset: last_len,
        };
    }

    // ---- edit operations ---------------------------------------------------

    /// Insert `s` at the caret and advance. Replaces the selection if any.
    /// Routes through the active region (body / header / footer).
    pub fn insert_str(&mut self, state: &mut ui::widget::Edits<'_>, s: &str) {
        if self
            .selection_anchor
            .as_ref()
            .is_none_or(|a| a.block == self.caret.block)
        {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            let edit = self.edit_core.insert(s);
            if self.apply_edit_core(state, &text, &edit) {
                return;
            }
        }
        self.delete_selection(state);
        if self.containers.is_none() {
            return;
        }
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        self.region_splice(state, &block, offset, 0, s);
        self.caret.offset = offset + s.chars().count();
    }

    /// Split the current block at the caret. Lists continue (the new block is a
    /// list item of the same kind); pressing Enter on an *empty* list item exits
    /// the list (the item becomes a paragraph). Headings split into a paragraph.
    /// Region-aware: in a header/footer the same split logic runs against that
    /// region's container.
    pub fn enter(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self.containers.is_none() {
            return;
        }
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        let text = self.region_block_text(state, &block).unwrap_or_default();
        let char_count = text.chars().count();
        let (kind, ordered, _level) = self.block_meta(state, &block);

        // Enter on an empty list item exits the list.
        if kind == "list_item" && char_count == 0 {
            self.region_set_kind(state, &block, "paragraph");
            return;
        }

        let new_kind = if kind == "list_item" {
            "list_item"
        } else {
            "paragraph"
        };
        let ordered = (new_kind == "list_item").then_some(ordered);
        let action = crate::actions::split_block_action(
            self.action_region(),
            &block,
            offset,
            new_kind,
            ordered,
        );
        if !self.apply_rich(state, action) {
            return;
        }
        let Some(id) = self
            .region_blocks_in_order(state)
            .into_iter()
            .skip_while(|id| id != &block)
            .nth(1)
        else {
            return;
        };
        self.caret = Caret {
            block: id,
            offset: 0,
        };
    }

    /// Delete the character before the caret; at offset 0 merge into the
    /// previous block. Region-aware.
    pub fn backspace(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self
            .selection_anchor
            .as_ref()
            .is_none_or(|a| a.block == self.caret.block)
        {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            let edit = self.edit_core.delete_backward(&text, false);
            if self.apply_edit_core(state, &text, &edit) {
                return;
            }
        }
        if self.delete_selection(state) {
            return;
        }
        if self.containers.is_none() {
            return;
        }
        let block = self.caret.block.clone();
        let offset = self.caret.offset;

        if offset > 0 {
            self.delete_grapheme(state, true);
            return;
        }

        // Backspace at the start of a list item removes the list style first.
        if self.block_meta(state, &block).0 == "list_item" {
            self.region_set_kind(state, &block, "paragraph");
            return;
        }

        // offset == 0: merge into the previous block.
        let blocks = self.ordered_blocks(state);
        let Some(pos) = blocks.iter().position(|b| b == &block) else {
            return;
        };
        if pos == 0 {
            return;
        }
        let Some(prev) = blocks.get(pos - 1).cloned() else {
            return;
        };
        let prev_len = self.block_len(state, &prev);
        let cur_text = self.region_block_text(state, &block).unwrap_or_default();
        if !cur_text.is_empty() {
            self.region_splice(state, &prev, prev_len, 0, &cur_text);
        }
        self.region_remove_block(state, &block);
        self.caret = Caret {
            block: prev,
            offset: prev_len,
        };
    }

    /// Delete the character after the caret; at end-of-block merge the next
    /// block in. Region-aware.
    pub fn delete_forward(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self
            .selection_anchor
            .as_ref()
            .is_none_or(|a| a.block == self.caret.block)
        {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            let edit = self.edit_core.delete_forward(&text, false);
            if self.apply_edit_core(state, &text, &edit) {
                return;
            }
        }
        if self.delete_selection(state) {
            return;
        }
        if self.containers.is_none() {
            return;
        }
        let block = self.caret.block.clone();
        let offset = self.caret.offset;
        let len = self.block_len(state, &block);

        if offset < len {
            self.delete_grapheme(state, false);
            return;
        }

        let blocks = self.ordered_blocks(state);
        let Some(pos) = blocks.iter().position(|b| b == &block) else {
            return;
        };
        if pos + 1 >= blocks.len() {
            return;
        }
        let Some(next) = blocks.get(pos + 1).cloned() else {
            return;
        };
        let next_text = self.region_block_text(state, &next).unwrap_or_default();
        if !next_text.is_empty() {
            self.region_splice(state, &block, len, 0, &next_text);
        }
        self.region_remove_block(state, &next);
        // caret stays at the merge point
    }

    // ---- caret motion ------------------------------------------------------

    pub fn move_left(&mut self, state: &AutoCommit) {
        let extends_selection = self
            .selection_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.block == self.caret.block);
        if extends_selection || self.caret.offset > 0 {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            self.edit_core
                .motion(&text, ui::text_edit::Motion::Left, extends_selection);
            self.sync_state_from_edit_core(&text);
            return;
        }
        let blocks = self.ordered_blocks(state);
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block)
            && pos > 0
            && let Some(prev) = blocks.get(pos - 1).cloned()
        {
            let len = self.block_len(state, &prev);
            self.caret = Caret {
                block: prev,
                offset: len,
            };
        }
    }

    pub fn move_right(&mut self, state: &AutoCommit) {
        let extends_selection = self
            .selection_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.block == self.caret.block);
        let len = self.block_len(state, &self.caret.block);
        if extends_selection || self.caret.offset < len {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            self.edit_core
                .motion(&text, ui::text_edit::Motion::Right, extends_selection);
            self.sync_state_from_edit_core(&text);
            return;
        }
        let blocks = self.ordered_blocks(state);
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block)
            && pos + 1 < blocks.len()
            && let Some(next) = blocks.get(pos + 1).cloned()
        {
            self.caret = Caret {
                block: next,
                offset: 0,
            };
        }
    }

    pub fn move_home(&mut self) {
        self.caret.offset = 0;
    }

    pub fn caret(&self) -> &Caret {
        &self.caret
    }

    pub fn set_caret(&mut self, caret: Caret) {
        self.caret = caret;
    }

    pub fn move_end(&mut self, state: &AutoCommit) {
        let extends_selection = self
            .selection_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.block == self.caret.block);
        if self.selection_anchor.is_none() || extends_selection {
            let text = self.current_text(state);
            self.sync_edit_core_from_state(&text);
            self.edit_core
                .motion(&text, ui::text_edit::Motion::LineEnd, extends_selection);
            self.sync_state_from_edit_core(&text);
            return;
        }
        self.caret.offset = self.block_len(state, &self.caret.block);
    }

    /// Move the caret up/down by `delta` blocks (block-granular Up/Down).
    pub fn move_block_relative(&mut self, state: &AutoCommit, delta: i32) {
        let blocks = self.ordered_blocks(state);
        if blocks.is_empty() {
            return;
        }
        if let Some(pos) = blocks.iter().position(|b| b == &self.caret.block) {
            let new = num::usize_of_i32(
                (num::i32_of_usize(pos) + delta).clamp(0, num::i32_of_usize(blocks.len()) - 1),
            );
            self.move_to_block(state, new);
        }
    }

    /// Move the caret to the block at `index` (clamped), preserving a target
    /// column where possible. Used by Up/Down once layout is available; here it
    /// is the block-granular fallback.
    pub fn move_to_block(&mut self, state: &AutoCommit, index: usize) {
        let blocks = self.ordered_blocks(state);
        if blocks.is_empty() {
            return;
        }
        let i = index.min(blocks.len() - 1);
        let Some(block) = blocks.get(i).cloned() else {
            return;
        };
        let len = self.block_len(state, &block);
        let offset = self.caret.offset.min(len);
        self.caret = Caret { block, offset };
    }

    /// The active selection as `((block, start), (block, end))`, ordered, **only**
    /// when it is a single-block selection (the header/footer band's v1 case) —
    /// `None` for no selection or a multi-block one. Used by the widget to paint
    /// the band selection without depending on layout.
    pub fn band_selection_range(&self) -> Option<((String, usize), (String, usize))> {
        let anchor = self.selection_anchor.as_ref()?;
        if anchor == &self.caret || anchor.block != self.caret.block {
            return None;
        }
        let (lo, hi) = if anchor.offset <= self.caret.offset {
            (anchor.offset, self.caret.offset)
        } else {
            (self.caret.offset, anchor.offset)
        };
        Some((
            (self.caret.block.clone(), lo),
            (self.caret.block.clone(), hi),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT};
    use tilo_automerge::ApplyIncoming;

    fn bound() -> (WordProcessor, AutoCommit) {
        let mut wp = WordProcessor::new();
        wp.attach(ui::framework::testing::detached_dispatcher("test-author"));
        let mut state = AutoCommit::new();
        crate::initialize_empty_document(&mut state).unwrap();
        wp.bind(&state, &ROOT);
        (wp, state)
    }

    fn assert_failed_import_preserves_document(format: crate::WpFormat) {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut Edits::lend(&mut state), "Saved draft");
        state.commit();
        // Do not commit this edit: recovery must retain the host's open transaction.
        wp.insert_str(&mut Edits::lend(&mut state), " plus unsaved work");
        wp.select_all(&state);
        let before_text = wp.block_texts(&state);
        let before_caret = wp.caret.clone();
        let before_anchor = wp.selection_anchor.clone();
        let before_pending = state.pending_ops();
        let before_bytes = state.clone().save();

        assert!(!wp.replace_with_format(
            &mut Edits::lend(&mut state),
            b"not a ZIP archive",
            format
        ));
        assert_eq!(wp.block_texts(&state), before_text);
        assert_eq!(wp.caret, before_caret);
        assert_eq!(wp.selection_anchor, before_anchor);
        assert_eq!(state.pending_ops(), before_pending);
        assert_eq!(state.clone().save(), before_bytes);

        // The failed attempt must not poison the block-id/order bookkeeping.
        wp.clear_anchor();
        wp.enter(&mut Edits::lend(&mut state));
        wp.insert_str(&mut Edits::lend(&mut state), "still editable");
        assert!(
            wp.block_texts(&state)
                .iter()
                .any(|text| text == "still editable")
        );
    }

    /// Corrupt docx and odt imports preserve the document; a successful import preserves the live document
    /// identity and commit boundary; bind creates the initial paragraph and caret, refreshes the body caret
    /// without order scans while clamping remote Unicode edits, accepts an empty byte order and recovers after
    /// the last body deletion, and rejects non-canonical body carets keeping region fallbacks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn imports_and_binding_keep_the_live_document_and_its_caret() {
        // Phase: corrupt_docx_import_preserves_document.
        {
            assert_failed_import_preserves_document(crate::WpFormat::Docx);
        }

        // Phase: corrupt_odt_import_preserves_document.
        {
            assert_failed_import_preserves_document(crate::WpFormat::Odt);
        }

        // Phase: successful_format_import_preserves_live_document_identity.
        {
            for format in [crate::WpFormat::Docx, crate::WpFormat::Odt] {
                let (mut source, mut source_state) = bound();
                source.insert_str(&mut Edits::lend(&mut source_state), "Imported document");
                let bytes = format
                    .export(
                        tilo_doc::DocRef {
                            doc: &source_state,
                            c: source.containers.as_ref().unwrap(),
                        },
                        &tilo_content::NoContent,
                    )
                    .unwrap();
                let (mut wp, mut state) = bound();
                state.put(ROOT, "host-marker", "preserve me").unwrap();
                wp.insert_str(&mut Edits::lend(&mut state), "Previous document");
                let heads = state.get_heads();
                let actor = state.get_actor().clone();
                let blocks = wp.containers.as_ref().unwrap().blocks.clone();

                assert!(wp.replace_with_format(&mut Edits::lend(&mut state), &bytes, format));
                assert_eq!(wp.block_texts(&state), vec!["Imported document"]);
                assert_eq!(state.get_actor(), &actor);
                assert_eq!(wp.containers.as_ref().unwrap().blocks, blocks);
                assert_eq!(
                    state.get(ROOT, "host-marker").unwrap().unwrap().0.as_str(),
                    Some("preserve me")
                );
                assert_eq!(
                    state.get_changes(&heads).len(),
                    1,
                    "replacement is one undoable change"
                );
            }
        }

        // Phase: bind_creates_initial_paragraph_and_caret.
        {
            let (wp, state) = bound();
            assert_eq!(wp.block_texts(&state), vec![String::new()]);
            assert_eq!(wp.caret.offset, 0);
            assert!(!wp.caret.block.is_empty());
        }

        // Phase: bound_body_caret_refresh_avoids_order_scans_and_clamps_remote_unicode_edits.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "abcdef");
            let anchor = Caret {
                block: wp.caret.block.clone(),
                offset: 1,
            };
            wp.selection_anchor = Some(anchor.clone());
            let before = tilo_doc::model::ordered_keys_scan_count();
            for _ in 0..3 {
                wp.bind(&state, &ROOT);
            }
            assert_eq!(
                tilo_doc::model::ordered_keys_scan_count(),
                before,
                "valid body caret must not enumerate the body"
            );
            let c = wp.containers.as_ref().unwrap();
            let (_, block) = state.get(&c.blocks, &wp.caret.block).unwrap().unwrap();
            let (_, text) = state
                .get(&block, tilo_doc::schema::BLOCK_TEXT)
                .unwrap()
                .unwrap();
            let mut remote = state.fork();
            remote.splice_text(&text, 0, 6, "é🦀").unwrap();
            state.merge_incoming(&mut remote).unwrap();
            let before = tilo_doc::model::ordered_keys_scan_count();
            wp.bind(&state, &ROOT);
            assert_eq!(tilo_doc::model::ordered_keys_scan_count(), before);
            assert_eq!(
                wp.caret.offset, 2,
                "clamp counts Unicode scalars, not bytes"
            );
            assert_eq!(
                wp.selection_anchor,
                Some(anchor),
                "valid caret retains existing selection behavior"
            );
            assert_eq!(wp.current_text(&state), "é🦀");
        }

        // Phase: bind_accepts_empty_byte_order_and_recovers_after_last_body_deletion.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "text");
            let c = wp.containers.as_ref().unwrap().clone();
            let id = wp.caret.block.clone();
            let (_, block) = state.get(&c.blocks, &id).unwrap().unwrap();
            state
                .put(
                    &block,
                    tilo_doc::schema::BLOCK_ORDER,
                    automerge::ScalarValue::Bytes(vec![]),
                )
                .unwrap();
            state.delete(&block, tilo_doc::schema::BLOCK_TEXT).unwrap();
            assert_eq!(
                tilo_doc::model::blocks_in_order(&state, &c),
                vec![id.clone()]
            );
            let before = tilo_doc::model::ordered_keys_scan_count();
            wp.bind(&state, &ROOT);
            assert_eq!(tilo_doc::model::ordered_keys_scan_count(), before);
            assert_eq!(
                wp.caret,
                Caret {
                    block: id.clone(),
                    offset: 0
                }
            );
            wp.selection_anchor = Some(wp.caret.clone());
            state.delete(&c.blocks, &id).unwrap();
            wp.bind(&state, &ROOT);
            assert!(
                tilo_doc::model::blocks_in_order(&state, &c).is_empty(),
                "a view never reseeds the body"
            );
        }

        // Phase: bind_rejects_noncanonical_body_carets_and_keeps_region_fallbacks.
        {
            for damage in ["deleted", "scalar_block", "missing_order", "scalar_order"] {
                let (mut wp, mut state) = bound();
                let first = wp.caret.block.clone();
                wp.insert_str(&mut Edits::lend(&mut state), "first");
                wp.enter(&mut Edits::lend(&mut state));
                wp.insert_str(&mut Edits::lend(&mut state), "second");
                wp.selection_anchor = Some(Caret {
                    block: first.clone(),
                    offset: 1,
                });
                let c = wp.containers.as_ref().unwrap().clone();
                let target = wp.caret.block.clone();
                let (_, block) = state.get(&c.blocks, &target).unwrap().unwrap();
                match damage {
                    "deleted" => state.delete(&c.blocks, &target).unwrap(),
                    "scalar_block" => state.put(&c.blocks, &target, "not a block").unwrap(),
                    "missing_order" => state.delete(&block, tilo_doc::schema::BLOCK_ORDER).unwrap(),
                    _ => state
                        .put(&block, tilo_doc::schema::BLOCK_ORDER, 42_i64)
                        .unwrap(),
                }
                let canonical = tilo_doc::model::blocks_in_order(&state, &c);
                assert!(!canonical.contains(&target));
                wp.bind(&state, &ROOT);
                assert_eq!(
                    wp.caret,
                    Caret {
                        block: canonical[0].clone(),
                        offset: 0
                    },
                    "{damage}"
                );
                assert_eq!(wp.selection_anchor, None, "{damage}");
            }
            for region in [Region::Header, Region::Footer] {
                let (mut wp, mut state) = bound();
                let body = wp.caret.block.clone();
                wp.focus_region(&mut Edits::lend(&mut state), region);
                wp.insert_str(&mut Edits::lend(&mut state), "running text");
                wp.bind(&state, &ROOT);
                assert_eq!(wp.region, region);
                assert_eq!(wp.caret.offset, 12);
                let caret_block = wp.caret.block.clone();
                let mut ids = wp.ids.clone().unwrap();
                tilo_doc::DocMut {
                    doc: &mut state,
                    c: wp.containers.as_ref().unwrap(),
                    ids: &mut ids,
                }
                .remove_block_in(region, &caret_block);
                wp.bind(&state, &ROOT);
                assert_eq!(wp.region, Region::Body);
                assert_eq!(
                    wp.caret,
                    Caret {
                        block: body,
                        offset: 0
                    }
                );
            }
        }
    }

    /// A new document resolves the built-in styles without writing them, and
    /// `set_stylesheet` persists through the container.
    #[test]
    fn stylesheets_resolve_builtins_and_persist_through_the_container() {
        // Phase: a_new_document_resolves_builtin_styles_without_writing_them.
        {
            let (wp, mut state) = bound();
            let heads = state.get_heads();
            let c = wp.containers.as_ref().unwrap();
            assert!(
                tilo_doc::model::style_ids(&state, c).is_empty(),
                "viewing a document writes no styles into it"
            );
            let sheet = wp.stylesheet(&state);
            for k in ["body", "heading1", "heading2", "heading3", "list", "quote"] {
                assert!(sheet.styles.contains_key(k), "built-in style {k} resolves");
            }
            assert_eq!(state.get_heads(), heads);
            assert_eq!(sheet.get("body").name, "Body");
            assert!(
                (sheet.get("body").size - 16.5).abs() < 1e-6,
                "the built-in body keeps its fractional size"
            );
            assert!(sheet.get("quote").italic, "quote stays italic");
        }

        // Phase: set_stylesheet_persists_through_the_container.
        {
            let (mut wp, mut state) = bound();
            let mut sheet = wp.stylesheet(&state);
            sheet.styles.get_mut("body").unwrap().size = 42.0;
            sheet.styles.insert(
                "custom".into(),
                StyleProps {
                    name: "Custom".into(),
                    font: "serif".into(),
                    size: 19.0,
                    ..Default::default()
                },
            );
            wp.set_stylesheet(&mut Edits::lend(&mut state), &sheet);

            // Read back via the container (a fresh read path, not the in-memory sheet).
            let back = wp.stylesheet(&state);
            assert_eq!(back.get("body").size, 42.0, "edited built-in persisted");
            assert_eq!(back.get("custom").name, "Custom", "new style persisted");
            assert_eq!(back.get("custom").size, 19.0);
        }
    }

    /// Typing advances the caret; Enter splits a block; Backspace works within and across blocks; Delete
    /// forward merges the next; caret motion crosses blocks and extends an active selection; selection delete
    /// and replace; select word picks the word under the caret.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn typing_and_deleting_edit_blocks_and_move_the_caret() {
        // Phase: type_text_advances_caret.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello");
            assert_eq!(wp.current_text(&state), "hello");
            assert_eq!(wp.caret.offset, 5);
        }

        // Phase: enter_splits_block.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "helloworld");
            // move caret back to between "hello" and "world"
            for _ in 0..5 {
                wp.move_left(&state);
            }
            assert_eq!(wp.caret.offset, 5);
            wp.enter(&mut Edits::lend(&mut state));
            assert_eq!(
                wp.block_texts(&state),
                vec!["hello".to_string(), "world".to_string()]
            );
            assert_eq!(wp.caret.offset, 0);
            assert_eq!(wp.current_text(&state), "world");
        }

        // Phase: backspace_within_and_across_blocks.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            assert_eq!(
                wp.block_texts(&state),
                vec!["ab".to_string(), "cd".to_string()]
            );

            // delete 'd'
            wp.backspace(&mut Edits::lend(&mut state));
            assert_eq!(wp.current_text(&state), "c");
            // delete 'c'
            wp.backspace(&mut Edits::lend(&mut state));
            assert_eq!(wp.current_text(&state), "");
            // now at offset 0 of second block → merge into first
            wp.backspace(&mut Edits::lend(&mut state));
            assert_eq!(wp.block_texts(&state), vec!["ab".to_string()]);
            assert_eq!(wp.caret.offset, 2, "caret lands at the merge point");
        }

        // Phase: delete_forward_merges_next.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            // caret at end of "cd"; move to end of first block
            wp.move_to_block(&state, 0);
            wp.move_end(&state);
            assert_eq!(wp.current_text(&state), "ab");
            wp.delete_forward(&mut Edits::lend(&mut state));
            assert_eq!(wp.block_texts(&state), vec!["abcd".to_string()]);
        }

        // Phase: caret_motion_crosses_blocks.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            // caret at end of "cd"; move_right at end → no-op (last block)
            wp.move_right(&state);
            assert_eq!(wp.current_text(&state), "cd");
            assert_eq!(wp.caret.offset, 2);
            // move_left from offset 0 of "cd" → end of "ab"
            wp.move_home();
            wp.move_left(&state);
            assert_eq!(wp.current_text(&state), "ab");
            assert_eq!(wp.caret.offset, 2);
        }

        // Phase: caret_motion_extends_an_active_selection.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "abcd");

            wp.move_home();
            wp.set_anchor();
            wp.move_right(&state);
            assert_eq!(
                wp.selection_blocks(&state)[0].1..wp.selection_blocks(&state)[0].2,
                0..1
            );

            wp.clear_anchor();
            wp.move_end(&state);
            wp.set_anchor();
            wp.move_home();
            assert_eq!(
                wp.selection_blocks(&state)[0].1..wp.selection_blocks(&state)[0].2,
                0..4
            );
        }

        // Phase: selection_delete_and_replace.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
            // select "hello" (0..5)
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            assert_eq!(
                wp.selection_blocks(&state)
                    .into_iter()
                    .map(|(_, s, e)| (s, e))
                    .collect::<Vec<_>>(),
                vec![(0, 5)]
            );
            // typing replaces the selection
            wp.insert_str(&mut Edits::lend(&mut state), "HI");
            assert_eq!(wp.current_text(&state), "HI world");
            assert_eq!(wp.caret.offset, 2);
            assert!(wp.selection_anchor.is_none());
        }

        // Phase: select_word_picks_word_under_caret.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world foo");
            wp.caret.offset = 7; // inside "world" (chars 6..11)
            wp.select_word(&state);
            let sel = wp.selection_blocks(&state);
            assert_eq!(sel.len(), 1);
            assert_eq!((sel[0].1, sel[0].2), (6, 11));
        }
    }

    /// Inline colour and font-size marks, highlight and toggled marks apply to the selection; super and sub
    /// are mutually exclusive; line spacing writes and reads back; alignment applies across selected
    /// paragraphs and cells; set align and paragraph style.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn marks_and_paragraph_attributes_apply_to_the_selection() {
        // Phase: inline_color_font_size_marks_apply_to_selection.
        {
            use tilo_doc::document::MarkValue;
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
            // select "hello" (0..5)
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            wp.set_text_color(&mut Edits::lend(&mut state), "#ff0000");
            wp.set_font_size(&mut Edits::lend(&mut state), 24);
            wp.set_font_family(&mut Edits::lend(&mut state), "serif");
            let c = wp.containers.as_ref().unwrap();
            let snap = tilo_doc::model::materialize(&state, c);
            let blk = &snap.blocks[0];
            let mark = |name: &str| {
                blk.marks
                    .iter()
                    .find(|m| m.name == name && m.start == 0 && m.end == 5)
                    .map(|m| m.value.clone())
            };
            match mark("color") {
                Some(MarkValue::Str(s)) => assert_eq!(s, "#ff0000"),
                _ => panic!("color mark missing over selection"),
            }
            match mark("size") {
                Some(MarkValue::Int(n)) => assert_eq!(n, 24),
                _ => panic!("size mark missing over selection"),
            }
            match mark("font") {
                Some(MarkValue::Str(s)) => assert_eq!(s, "serif"),
                _ => panic!("font mark missing over selection"),
            }
        }

        // Phase: highlight_mark_applies_to_selection.
        {
            use tilo_doc::document::MarkValue;
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            wp.set_highlight(&mut Edits::lend(&mut state), "#fff35c");
            let c = wp.containers.as_ref().unwrap();
            let snap = tilo_doc::model::materialize(&state, c);
            let blk = &snap.blocks[0];
            match blk
                .marks
                .iter()
                .find(|m| m.name == "highlight" && m.start == 0 && m.end == 5)
                .map(|m| m.value.clone())
            {
                Some(MarkValue::Str(s)) => assert_eq!(s, "#fff35c"),
                _ => panic!("highlight mark missing over selection"),
            }
            // Clearing removes it.
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            wp.clear_mark(&mut Edits::lend(&mut state), "highlight");
            let snap = tilo_doc::model::materialize(&state, wp.containers.as_ref().unwrap());
            assert!(
                !snap.blocks[0].marks.iter().any(|m| m.name == "highlight"),
                "highlight cleared"
            );
        }

        // Phase: super_and_sub_are_mutually_exclusive.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "x2");
            // Select "2".
            wp.caret.offset = 1;
            wp.set_anchor();
            wp.caret.offset = 2;
            wp.toggle_super(&mut Edits::lend(&mut state));
            let c = wp.containers.clone().unwrap();
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &wp.caret.block, 1, 2, "super"),
                "super applied"
            );
            assert!(
                !tilo_doc::model::has_mark_over(&state, &c, &wp.caret.block, 1, 2, "sub"),
                "no sub yet"
            );

            // Re-select and toggle sub: it must clear super (mutual exclusion).
            wp.caret.offset = 1;
            wp.set_anchor();
            wp.caret.offset = 2;
            wp.toggle_sub(&mut Edits::lend(&mut state));
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &wp.caret.block, 1, 2, "sub"),
                "sub applied"
            );
            assert!(
                !tilo_doc::model::has_mark_over(&state, &c, &wp.caret.block, 1, 2, "super"),
                "super cleared"
            );

            // Toggling sub again removes it (off).
            wp.caret.offset = 1;
            wp.set_anchor();
            wp.caret.offset = 2;
            wp.toggle_sub(&mut Edits::lend(&mut state));
            assert!(
                !tilo_doc::model::has_mark_over(&state, &c, &wp.caret.block, 1, 2, "sub"),
                "sub toggled off"
            );
        }

        // Phase: line_spacing_writes_block_attr_and_reads_back.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "a paragraph");
            assert!(
                (wp.caret_line_spacing(&state) - 1.0).abs() < 1e-9,
                "default 1.0 when unset"
            );
            wp.set_line_spacing(&mut Edits::lend(&mut state), 2.0);
            let c = wp.containers.as_ref().unwrap();
            let snap = tilo_doc::model::materialize(&state, c);
            match snap.blocks[0].attrs.get("line_spacing") {
                Some(tilo_doc::AttrVal::Float(f)) => {
                    assert!((f - 2.0).abs() < 1e-9, "line_spacing=2.0 stored");
                }
                other => panic!("expected line_spacing Float attr, got {other:?}"),
            }
            assert!(
                (wp.caret_line_spacing(&state) - 2.0).abs() < 1e-9,
                "caret reads 2.0 back"
            );
        }

        // Phase: align_applies_across_selected_paragraphs.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "one");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "two");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "three");
            let blocks = wp.ordered_blocks(&state);
            assert_eq!(blocks.len(), 3);
            wp.set_caret(Caret {
                block: blocks[0].clone(),
                offset: 0,
            });
            wp.set_anchor();
            wp.set_caret(Caret {
                block: blocks[2].clone(),
                offset: 5,
            });
            wp.set_align(&mut Edits::lend(&mut state), "center");
            for b in &blocks {
                assert_eq!(
                    align_of(&wp, &state, b).as_deref(),
                    Some("center"),
                    "paragraph {b} centered"
                );
            }
        }

        // Phase: align_applies_across_selected_cells.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            let snap = tilo_doc::model::materialize(&state, wp.containers.as_ref().unwrap());
            let cells: Vec<String> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "cell")
                .map(|b| b.id.clone())
                .collect();
            assert_eq!(cells.len(), 4);
            wp.set_caret(Caret {
                block: cells[0].clone(),
                offset: 0,
            });
            wp.set_anchor();
            wp.set_caret(Caret {
                block: cells[3].clone(),
                offset: 0,
            });
            wp.set_align(&mut Edits::lend(&mut state), "center");
            for c in &cells {
                assert_eq!(
                    align_of(&wp, &state, c).as_deref(),
                    Some("center"),
                    "cell {c} centered"
                );
            }
        }

        // Phase: toggle_mark_adds_then_removes.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello");
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            let c = wp.containers.clone().unwrap();
            let block = wp.caret.block.clone();

            wp.toggle_mark(&mut Edits::lend(&mut state), "strong");
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"),
                "mark applied"
            );
            wp.toggle_mark(&mut Edits::lend(&mut state), "strong");
            assert!(
                !tilo_doc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"),
                "mark toggled off"
            );
        }

        // Phase: set_align_and_paragraph_style.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "Title");
            wp.set_align(&mut Edits::lend(&mut state), "center");
            wp.set_paragraph_style(&mut Edits::lend(&mut state), "heading", Some(2), None);
            let c = wp.containers.clone().unwrap();
            let snap = tilo_doc::model::materialize(&state, &c);
            assert_eq!(snap.blocks[0].kind, "heading");
            assert_eq!(
                snap.blocks[0].attrs.get("level"),
                Some(&tilo_doc::AttrVal::Int(2))
            );
            assert_eq!(
                snap.blocks[0].attrs.get("align"),
                Some(&tilo_doc::AttrVal::Str("center".into()))
            );
        }
    }

    /// Multi-block selection copy and delete; multiline insert splits into paragraphs; rich copy/paste
    /// preserves formatting; Markdown prefixes autoformat; Enter continues and exits lists; Backspace at list
    /// start unlists.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn clipboard_and_multiline_input_keep_structure_and_formatting() {
        // Phase: multi_block_selection_copy_and_delete.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "first");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "second");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "third");
            let blocks = wp.ordered_blocks(&state);
            assert_eq!(blocks.len(), 3);

            // Select from "rst" of block 0 through "thi" of block 2.
            wp.set_caret(Caret {
                block: blocks[0].clone(),
                offset: 2,
            });
            wp.set_anchor();
            wp.set_caret(Caret {
                block: blocks[2].clone(),
                offset: 3,
            });

            // Selected text spans three blocks, joined by newlines.
            assert_eq!(wp.selection_text(&state), "rst\nsecond\nthi");

            // Deleting the selection merges the surviving heads/tails into one block.
            assert!(wp.delete_selection(&mut Edits::lend(&mut state)));
            assert_eq!(wp.block_texts(&state), vec!["fird".to_string()]);
            assert!(!wp.has_selection());
        }

        // Phase: insert_multiline_splits_into_paragraphs.
        {
            let (mut wp, mut state) = bound();
            wp.insert_multiline(&mut Edits::lend(&mut state), "alpha\nbeta\ngamma");
            assert_eq!(
                wp.block_texts(&state),
                vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
            );
        }

        // Phase: rich_copy_paste_preserves_formatting.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
            // bold "hello"
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            wp.toggle_mark(&mut Edits::lend(&mut state), "strong");

            // re-select "hello" and copy a rich fragment
            wp.caret.offset = 0;
            wp.clear_anchor();
            wp.set_anchor();
            wp.caret.offset = 5;
            let (plain, blocks) = wp.copy_fragment(&state);
            assert_eq!(plain, "hello");
            assert_eq!(blocks.len(), 1);
            assert!(
                blocks[0]
                    .marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 0 && m.end == 5)
            );

            // paste rich at the end → the pasted run keeps its bold
            wp.clear_anchor();
            wp.move_end(&state);
            wp.insert_rich(&mut Edits::lend(&mut state), &blocks);
            let c = wp.containers.clone().unwrap();
            let block = wp.caret.block.clone();
            assert_eq!(wp.current_text(&state), "hello worldhello");
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &block, 11, 16, "strong"),
                "pasted bold preserved"
            );
        }

        // Phase: autoformat_markdown_prefixes.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "-");
            assert!(
                wp.autoformat_on_space(&mut Edits::lend(&mut state)),
                "'- ' autoformatted"
            );
            let b = wp.caret.block.clone();
            let (kind, ordered, _) = wp.block_meta(&state, &b);
            assert_eq!(kind, "list_item");
            assert!(!ordered, "dash → bullet");
            assert_eq!(wp.current_text(&state), "", "prefix consumed");
            assert_eq!(wp.caret.offset, 0);

            // "## " → heading 2
            wp.set_paragraph_style(&mut Edits::lend(&mut state), "paragraph", None, None);
            wp.move_end(&state);
            wp.insert_str(&mut Edits::lend(&mut state), "##");
            assert!(wp.autoformat_on_space(&mut Edits::lend(&mut state)));
            let b = wp.caret.block.clone();
            let (kind, _, level) = wp.block_meta(&state, &b);
            assert_eq!(kind, "heading");
            assert_eq!(level, Some(2));

            // No false positive in the middle of text.
            let (mut wp2, mut s2) = bound();
            wp2.insert_str(&mut Edits::lend(&mut s2), "a-");
            assert!(
                !wp2.autoformat_on_space(&mut Edits::lend(&mut s2)),
                "mid-line dash not autoformatted"
            );
        }

        // Phase: enter_continues_and_exits_lists.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "-");
            wp.autoformat_on_space(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "first");
            wp.enter(&mut Edits::lend(&mut state));
            let b = wp.caret.block.clone();
            assert_eq!(
                wp.block_meta(&state, &b).0,
                "list_item",
                "Enter continues the list"
            );
            wp.enter(&mut Edits::lend(&mut state)); // empty item → exit
            let b = wp.caret.block.clone();
            assert_eq!(
                wp.block_meta(&state, &b).0,
                "paragraph",
                "Enter on empty item exits the list"
            );
        }

        // Phase: backspace_at_list_start_unlists.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "-");
            wp.autoformat_on_space(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "item");
            wp.move_home();
            wp.backspace(&mut Edits::lend(&mut state));
            let b = wp.caret.block.clone();
            assert_eq!(
                wp.block_meta(&state, &b).0,
                "paragraph",
                "backspace at list start un-lists"
            );
            assert_eq!(wp.current_text(&state), "item", "text preserved");
        }
    }

    fn align_of(wp: &WordProcessor, state: &AutoCommit, id: &str) -> Option<String> {
        let c = wp.containers.as_ref().unwrap();
        tilo_doc::model::materialize(state, c)
            .blocks
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| match b.attrs.get("align") {
                Some(tilo_doc::AttrVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
    }

    /// Table rows and columns add and delete; an inserted table has editable cells; inserted image bytes
    /// round-trip blob and dims and split text around the caret; inserted rows and columns keep the table
    /// rectangular; deleting the last row removes the table.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn tables_and_images_stay_rectangular_and_editable() {
        // Phase: table_add_and_delete_rows_cols.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            let count = |wp: &WordProcessor, st: &AutoCommit| {
                tilo_doc::model::materialize(st, wp.containers.as_ref().unwrap())
                    .blocks
                    .iter()
                    .filter(|b| b.kind == "cell")
                    .count()
            };
            assert_eq!(count(&wp, &state), 4);
            wp.table_add_row(&mut Edits::lend(&mut state));
            assert_eq!(count(&wp, &state), 6);
            wp.table_add_col(&mut Edits::lend(&mut state));
            assert_eq!(count(&wp, &state), 9);
            wp.table_del_row(&mut Edits::lend(&mut state));
            assert_eq!(count(&wp, &state), 6);
            wp.table_del_col(&mut Edits::lend(&mut state));
            assert_eq!(count(&wp, &state), 4);
            wp.table_delete(&mut Edits::lend(&mut state));
            assert_eq!(count(&wp, &state), 0);
        }

        // Phase: insert_table_creates_editable_cells.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "before");
            wp.insert_table(&mut Edits::lend(&mut state), 2, 3);
            let snap = tilo_doc::model::materialize(&state, wp.containers.as_ref().unwrap());
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(cells.len(), 6, "2x3 = 6 cells");
            // every cell shares one table id; rows/cols are tagged
            let tid = match cells[0].attrs.get("table") {
                Some(tilo_doc::AttrVal::Str(s)) => s.clone(),
                other => panic!("cell missing table attr: {other:?}"),
            };
            assert!(
                cells
                    .iter()
                    .all(|c| c.attrs.get("table") == Some(&tilo_doc::AttrVal::Str(tid.clone())))
            );
            // caret landed in the first cell; typing edits it
            assert!(cells.iter().any(|c| c.id == wp.caret.block));
            wp.insert_str(&mut Edits::lend(&mut state), "A1");
            let snap2 = tilo_doc::model::materialize(&state, wp.containers.as_ref().unwrap());
            let first = snap2
                .blocks
                .iter()
                .find(|b| b.id == wp.caret.block)
                .unwrap();
            assert_eq!(first.text, "A1");
        }

        // Phase: insert_image_bytes_round_trips_blob_and_dims.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "above");
            wp.insert_image_bytes(&mut Edits::lend(&mut state), TINY_PNG);

            let c = wp.containers.clone().unwrap();
            let snap = tilo_doc::model::materialize(&state, &c);
            let img = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .expect("an image block was inserted");

            // The blob attr references a blob whose bytes round-trip exactly.
            let hash = match img.attrs.get("blob") {
                Some(tilo_doc::AttrVal::Str(s)) => s.clone(),
                other => panic!("image block missing blob attr: {other:?}"),
            };
            assert_eq!(
                wp.content_store().read(&hash).as_deref(),
                Some(TINY_PNG),
                "image bytes round-trip through the content store"
            );

            // Intrinsic pixel dimensions were recorded (mirrors the ODF import path).
            assert_eq!(img.attrs.get("width"), Some(&tilo_doc::AttrVal::Int(2)));
            assert_eq!(img.attrs.get("height"), Some(&tilo_doc::AttrVal::Int(3)));

            // The image landed after the original text, and the caret sits in a fresh
            // paragraph below it so typing continues beneath the image.
            let order = wp.ordered_blocks(&state);
            let img_pos = order.iter().position(|id| id == &img.id).unwrap();
            assert!(img_pos >= 1, "image inserted after the original paragraph");
            assert_eq!(wp.caret.offset, 0);
            assert_ne!(wp.caret.block, img.id, "caret moved off the image block");
        }

        // Phase: insert_image_splits_text_around_caret.
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "helloworld");
            // caret between "hello" and "world"
            for _ in 0..5 {
                wp.move_left(&state);
            }
            wp.insert_image_bytes(&mut Edits::lend(&mut state), TINY_PNG);
            let texts = wp.block_texts(&state);
            // head paragraph, image (empty text), tail paragraph
            assert_eq!(
                texts,
                vec!["hello".to_string(), String::new(), "world".to_string()]
            );
            assert_eq!(
                wp.current_text(&state),
                "world",
                "caret follows the tail below the image"
            );
        }

        // Phase: insert_row_grows_table_and_stays_rectangular.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 3);
            assert_rectangular(&table_grid(&wp, &state), 2, 3);
            // Caret is in the first cell (row 0) after insert_table.
            wp.insert_row_below(&mut Edits::lend(&mut state));
            let grid = table_grid(&wp, &state);
            let rows = grid.iter().map(|(r, _)| *r).max().unwrap() + 1;
            assert_eq!(rows, 3, "row count increased by one");
            assert_rectangular(&grid, 3, 3);
            // The caret landed on the freshly-inserted row (row 1, below row 0).
            let (cr, _) = wp.caret_rc(&wp.caret_table(&state).unwrap().1);
            assert_eq!(cr, 1, "caret moved into the new row");
        }

        // Phase: insert_row_above_inserts_before_caret_row.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            // Move caret to row 1.
            let (_, cells) = wp.caret_table(&state).unwrap();
            let r1c0 = cells
                .iter()
                .find(|(_, r, c)| *r == 1 && *c == 0)
                .unwrap()
                .0
                .clone();
            wp.set_caret(Caret {
                block: r1c0,
                offset: 0,
            });
            wp.insert_row_above(&mut Edits::lend(&mut state));
            assert_rectangular(&table_grid(&wp, &state), 3, 2);
            let (cr, _) = wp.caret_rc(&wp.caret_table(&state).unwrap().1);
            assert_eq!(cr, 1, "the new row took row 1's slot; caret is on it");
        }

        // Phase: delete_col_removes_one_cell_per_row.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 3, 3);
            let before = table_grid(&wp, &state).len();
            assert_eq!(before, 9);
            wp.table_del_col(&mut Edits::lend(&mut state));
            let grid = table_grid(&wp, &state);
            assert_eq!(grid.len(), before - 3, "exactly one cell removed per row");
            assert_rectangular(&grid, 3, 2);
        }

        // Phase: insert_col_left_and_right_stay_rectangular.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            // Move to col 1 so left/right are unambiguous.
            let (_, cells) = wp.caret_table(&state).unwrap();
            let r0c1 = cells
                .iter()
                .find(|(_, r, c)| *r == 0 && *c == 1)
                .unwrap()
                .0
                .clone();
            wp.set_caret(Caret {
                block: r0c1,
                offset: 0,
            });
            wp.insert_col_left(&mut Edits::lend(&mut state));
            assert_rectangular(&table_grid(&wp, &state), 2, 3);
            wp.insert_col_right(&mut Edits::lend(&mut state));
            assert_rectangular(&table_grid(&wp, &state), 2, 4);
        }

        // Phase: delete_last_row_removes_table.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 1, 2);
            wp.table_del_row(&mut Edits::lend(&mut state));
            let c = wp.containers.as_ref().unwrap();
            let snap = tilo_doc::model::materialize(&state, c);
            assert!(
                snap.blocks.iter().all(|b| b.kind != "cell"),
                "deleting the last row removed the whole table"
            );
            assert!(!wp.caret_in_cell(&state), "caret is no longer in a cell");
        }
    }

    /// A minimal 2×3 RGB PNG (red pixels) — small enough to embed yet a real,
    /// decodable image so intrinsic-size attrs can be asserted.
    const TINY_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2,
        0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174,
        66, 96, 130,
    ];

    // ---- table editing -----------------------------------------------------

    /// The materialized `(row, col)` grid coordinates of every cell in the single
    /// table the snapshot contains, sorted row-major. Reads the production
    /// snapshot path so tests assert on what collaborators/exporters actually see.
    fn table_grid(wp: &WordProcessor, state: &AutoCommit) -> Vec<(usize, usize)> {
        let c = wp.containers.as_ref().unwrap();
        let snap = tilo_doc::model::materialize(state, c);
        let int = |b: &tilo_doc::snapshot::BlockView, k: &str| match b.attrs.get(k) {
            Some(tilo_doc::AttrVal::Int(n)) if *n >= 0 => num::usize_of_i64(*n),
            _ => 0,
        };
        let mut g: Vec<(usize, usize)> = snap
            .blocks
            .iter()
            .filter(|b| b.kind == "cell")
            .map(|b| (int(b, "row"), int(b, "col")))
            .collect();
        g.sort_unstable();
        g
    }

    /// Assert a grid is a full `rows × cols` rectangle (every coordinate present
    /// exactly once) — the table-rectangularity invariant.
    fn assert_rectangular(grid: &[(usize, usize)], rows: usize, cols: usize) {
        let mut expected: Vec<(usize, usize)> = (0..rows)
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .collect();
        expected.sort_unstable();
        assert_eq!(
            grid,
            expected.as_slice(),
            "table is a full {rows}×{cols} rectangle"
        );
    }

    /// Tab walks cells leaving structural append to the action host, Shift-Tab walks backwards, and vertical
    /// arrows move between cells.
    #[test]
    fn table_navigation_walks_cells_and_leaves_structure_to_the_host() {
        // Phase: tab_walks_cells_but_leaves_structural_append_to_the_action_host.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2); // caret in (0,0)
            // Tab forward through every cell: (0,0)->(0,1)->(1,0)->(1,1).
            for expect in [(0, 1), (1, 0), (1, 1)] {
                assert!(
                    wp.tab_cell(&mut Edits::lend(&mut state), true),
                    "tab handled inside the table"
                );
                assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), expect);
            }
            // The editing core deliberately does not perform a hidden structural
            // mutation at the edge. The widget detects this position and dispatches
            // an absolute `EditTable::InsertRow` through the shared action host.
            assert!(wp.tab_cell(&mut Edits::lend(&mut state), true));
            let grid = table_grid(&wp, &state);
            assert_rectangular(&grid, 2, 2);
            assert_eq!(
                wp.caret_rc(&wp.caret_table(&state).unwrap().1),
                (1, 1),
                "caret stays at the stable edge target for the action host"
            );
        }

        // Phase: shift_tab_walks_backwards.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            // Jump to the last cell, then Shift-Tab back one.
            let (_, cells) = wp.caret_table(&state).unwrap();
            let last = cells
                .iter()
                .find(|(_, r, c)| *r == 1 && *c == 1)
                .unwrap()
                .0
                .clone();
            wp.set_caret(Caret {
                block: last,
                offset: 0,
            });
            assert!(wp.tab_cell(&mut Edits::lend(&mut state), false));
            assert_eq!(
                wp.caret_rc(&wp.caret_table(&state).unwrap().1),
                (1, 0),
                "shift-tab moved to the previous cell"
            );
        }

        // Phase: vertical_arrows_move_between_cells.
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2); // caret in (0,0)
            assert!(wp.cell_vertical(&state, true), "down moved into a cell");
            assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), (1, 0));
            assert!(wp.cell_vertical(&state, false), "up moved back");
            assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), (0, 0));
            // Up from the top row has no cell above → not handled here.
            assert!(
                !wp.cell_vertical(&state, false),
                "no cell above the top row"
            );
        }
    }

    /// Build a small document with three headings of differing levels and a body
    /// paragraph between them, returning `(wp, state)`.
    fn doc_with_headings() -> (WordProcessor, AutoCommit) {
        let (mut wp, mut state) = bound();
        wp.insert_str(&mut Edits::lend(&mut state), "Introduction");
        wp.set_paragraph_style(&mut Edits::lend(&mut state), "heading", Some(1), None);
        wp.enter(&mut Edits::lend(&mut state));
        wp.set_paragraph_style(&mut Edits::lend(&mut state), "paragraph", None, None);
        wp.insert_str(&mut Edits::lend(&mut state), "Body text here.");
        wp.enter(&mut Edits::lend(&mut state));
        wp.insert_str(&mut Edits::lend(&mut state), "Background");
        wp.set_paragraph_style(&mut Edits::lend(&mut state), "heading", Some(2), None);
        wp.enter(&mut Edits::lend(&mut state));
        wp.set_paragraph_style(&mut Edits::lend(&mut state), "paragraph", None, None);
        wp.insert_str(&mut Edits::lend(&mut state), "More body.");
        wp.enter(&mut Edits::lend(&mut state));
        wp.insert_str(&mut Edits::lend(&mut state), "Conclusion");
        wp.set_paragraph_style(&mut Edits::lend(&mut state), "heading", Some(1), None);
        (wp, state)
    }

    /// Headings list one entry per heading in order with levels; generating a TOC inserts one block per
    /// heading at the top and regenerating replaces without duplicating.
    #[test]
    fn headings_drive_the_outline_and_a_non_duplicating_toc() {
        // Phase: headings_lists_one_entry_per_heading_in_order_with_levels.
        {
            let (wp, state) = doc_with_headings();
            let h = wp.headings(&state);
            assert_eq!(h.len(), 3, "one outline entry per heading block");
            assert_eq!(
                h.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
                vec!["Introduction", "Background", "Conclusion"],
                "headings in document order"
            );
            assert_eq!(
                h.iter().map(|e| e.level).collect::<Vec<_>>(),
                vec![1, 2, 1],
                "levels preserved per heading"
            );
        }

        // Phase: generate_toc_inserts_one_block_per_heading_at_top.
        {
            let (mut wp, mut state) = doc_with_headings();
            let entries: Vec<crate::actions::TocTarget> = wp
                .headings(&state)
                .iter()
                .map(|h| crate::actions::TocTarget {
                    heading: h.id.clone(),
                    level: h.level,
                    text: h.text.clone(),
                    page: Some(1),
                })
                .collect();
            let inserted = wp.generate_toc(&mut Edits::lend(&mut state), &entries);
            assert_eq!(inserted.len(), 3, "one TOC block per heading");

            let c = wp.containers.clone().unwrap();
            let snap = tilo_doc::model::materialize(&state, &c);
            let toc_count = snap.blocks.iter().filter(|b| block_is_toc(b)).count();
            assert_eq!(toc_count, 3, "exactly three toc-marked blocks exist");
            // They sit at the very top of the document, in heading order.
            assert_eq!(
                &snap.blocks[0].id, &inserted[0],
                "TOC begins at the document top"
            );
            assert!(snap.blocks[0].text.contains("Introduction"));
            assert!(snap.blocks[1].text.contains("Background"));
            assert!(snap.blocks[2].text.contains("Conclusion"));
            // The page number appears on each line.
            assert!(
                snap.blocks[0].text.trim_end().ends_with('1'),
                "line carries the page number"
            );
        }

        // Phase: regenerating_toc_replaces_without_duplicating.
        {
            let (mut wp, mut state) = doc_with_headings();
            let entries: Vec<crate::actions::TocTarget> = wp
                .headings(&state)
                .iter()
                .map(|h| crate::actions::TocTarget {
                    heading: h.id.clone(),
                    level: h.level,
                    text: h.text.clone(),
                    page: Some(2),
                })
                .collect();
            wp.generate_toc(&mut Edits::lend(&mut state), &entries);
            // Regenerate from the (now TOC-prefixed) document: headings() excludes the
            // TOC's own lines, so the entry set is unchanged.
            let entries2: Vec<crate::actions::TocTarget> = wp
                .headings(&state)
                .iter()
                .map(|h| crate::actions::TocTarget {
                    heading: h.id.clone(),
                    level: h.level,
                    text: h.text.clone(),
                    page: Some(2),
                })
                .collect();
            assert_eq!(
                entries2.len(),
                3,
                "TOC lines are not mistaken for headings on regen"
            );
            wp.generate_toc(&mut Edits::lend(&mut state), &entries2);

            let c = wp.containers.clone().unwrap();
            let snap = tilo_doc::model::materialize(&state, &c);
            let toc_count = snap.blocks.iter().filter(|b| block_is_toc(b)).count();
            assert_eq!(
                toc_count, 3,
                "regenerating replaces the old TOC (no duplicate)"
            );
            let heading_count = snap.blocks.iter().filter(|b| b.kind == "heading").count();
            assert_eq!(
                heading_count, 3,
                "headings are untouched by TOC regeneration"
            );
        }
    }

    // ---- editable headers / footers (edit-context routing) -----------------

    fn header_texts(wp: &WordProcessor, state: &AutoCommit) -> Vec<String> {
        let c = wp.containers.as_ref().unwrap();
        tilo_doc::model::materialize_region(state, c, Region::Header)
            .into_iter()
            .map(|b| b.text)
            .collect()
    }
    fn footer_texts(wp: &WordProcessor, state: &AutoCommit) -> Vec<String> {
        let c = wp.containers.as_ref().unwrap();
        tilo_doc::model::materialize_region(state, c, Region::Footer)
            .into_iter()
            .map(|b| b.text)
            .collect()
    }
    fn body_texts(wp: &WordProcessor, state: &AutoCommit) -> Vec<String> {
        let c = wp.containers.as_ref().unwrap();
        tilo_doc::model::materialize(state, c)
            .blocks
            .into_iter()
            .map(|b| b.text)
            .collect()
    }

    /// Focusing the header routes typing into it and switching back edits the body; footer Enter splits
    /// footer blocks; header Backspace works within and merges blocks; header bold routes to the header block;
    /// a page number in the footer writes a field mark; body editing is unchanged by the region machinery.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn header_and_footer_regions_edit_independently_of_the_body() {
        // Phase: focusing_header_and_typing_writes_into_header_not_body.
        {
            let (mut wp, mut state) = bound();
            // Body has its seeded empty paragraph.
            wp.insert_str(&mut Edits::lend(&mut state), "body-content");
            assert_eq!(wp.region(), Region::Body);

            // Focus the header context — an empty header paragraph is created lazily.
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            assert_eq!(wp.region(), Region::Header);
            assert!(wp.in_running_region());
            wp.insert_str(&mut Edits::lend(&mut state), "My Title");

            // The text lands in the HEADER container, asserted on the materialized snap.
            assert_eq!(header_texts(&wp, &state), vec!["My Title".to_string()]);
            // The body is untouched.
            assert_eq!(body_texts(&wp, &state), vec!["body-content".to_string()]);
        }

        // Phase: switching_back_to_body_edits_the_body_again.
        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "Head");
            // Return to the body and type — edits go to the body, header preserved.
            wp.focus_body(&mut Edits::lend(&mut state));
            assert_eq!(wp.region(), Region::Body);
            wp.insert_str(&mut Edits::lend(&mut state), "Body");
            assert_eq!(body_texts(&wp, &state), vec!["Body".to_string()]);
            assert_eq!(header_texts(&wp, &state), vec!["Head".to_string()]);
            // Returning to the header restores its caret (end of "Head") and appends.
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "er");
            assert_eq!(header_texts(&wp, &state), vec!["Header".to_string()]);
        }

        // Phase: footer_enter_splits_into_two_footer_blocks.
        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Footer);
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            assert_eq!(
                footer_texts(&wp, &state),
                vec!["ab".to_string(), "cd".to_string()]
            );
            // Body never gained a block beyond its seeded empty paragraph.
            assert_eq!(body_texts(&wp, &state), vec![String::new()]);
        }

        // Phase: header_backspace_within_and_merges_blocks.
        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            assert_eq!(
                header_texts(&wp, &state),
                vec!["ab".to_string(), "cd".to_string()]
            );
            // Delete 'd', 'c', then merge into the previous header block.
            wp.backspace(&mut Edits::lend(&mut state));
            wp.backspace(&mut Edits::lend(&mut state));
            assert_eq!(
                header_texts(&wp, &state),
                vec!["ab".to_string(), String::new()]
            );
            wp.backspace(&mut Edits::lend(&mut state));
            assert_eq!(header_texts(&wp, &state), vec!["ab".to_string()]);
            assert_eq!(
                wp.caret.offset, 2,
                "caret lands at the merge point in the header"
            );
        }

        // Phase: header_bold_mark_routes_to_the_header_block.
        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "Title");
            // Select the whole word, then toggle bold.
            wp.select_all(&state);
            wp.toggle_mark(&mut Edits::lend(&mut state), "strong");

            let c = wp.containers.as_ref().unwrap();
            let hv = tilo_doc::model::materialize_region(&state, c, Region::Header);
            let marks = &hv[0].marks;
            assert!(
                marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 0 && m.end >= 5),
                "bold mark applied to the header run: {marks:?}"
            );
            // Toggling again clears it (uses the region-aware unmark path).
            wp.select_all(&state);
            wp.toggle_mark(&mut Edits::lend(&mut state), "strong");
            let hv2 = tilo_doc::model::materialize_region(
                &state,
                wp.containers.as_ref().unwrap(),
                Region::Header,
            );
            assert!(
                !hv2[0].marks.iter().any(|m| m.name == "strong"),
                "bold mark cleared in the header"
            );
        }

        // Phase: insert_page_number_in_footer_writes_a_field_mark.
        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Footer);
            wp.insert_str(&mut Edits::lend(&mut state), "Page ");
            wp.insert_page_number(&mut Edits::lend(&mut state));

            let c = wp.containers.as_ref().unwrap();
            let fv = tilo_doc::model::materialize_region(&state, c, Region::Footer);
            assert!(
                fv[0].text.starts_with("Page "),
                "footer keeps its literal text: {:?}",
                fv[0].text
            );
            assert!(
                fv[0]
                    .marks
                    .iter()
                    .any(|m| m.name == tilo_doc::schema::MARK_FIELD),
                "a page-number field mark is present in the footer: {:?}",
                fv[0].marks
            );
        }

        // Phase: body_editing_is_unchanged_by_the_region_machinery.
        {
            // Regression guard: with the default Body context the ops behave exactly
            // as before (the body path must stay intact).
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "helloworld");
            for _ in 0..5 {
                wp.move_left(&state);
            }
            wp.enter(&mut Edits::lend(&mut state));
            assert_eq!(
                wp.block_texts(&state),
                vec!["hello".to_string(), "world".to_string()]
            );
            // No header/footer were created by body editing.
            assert!(header_texts(&wp, &state).is_empty());
            assert!(footer_texts(&wp, &state).is_empty());
        }
    }
}
