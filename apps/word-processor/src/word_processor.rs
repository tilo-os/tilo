//! The editing model over a `tilo-doc` document: a caret of `(block id, character offset)`
//! and the keyboard edits, as mutations of the canonical document.

use crate::num;
use crate::styles::{StyleProps, StyleSheet};
use automerge::{AutoCommit, ObjId, ReadDoc};
use tilo_doc::model::Region;
use tilo_doc::{Containers, IdGen};
use ui::text_edit::EditCore;
#[cfg(test)]
use ui::widget::Edits;

#[cfg(test)]
mod fixtures;

pub type BlockSpans = Vec<(String, usize, usize)>;

/// A caret: a block id and a character offset into that block's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caret {
    pub block: String,
    pub offset: usize,
}

/// Order a caret and anchor by block index, as both the model and the widget compare them.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub id: String,
    pub level: i64,
    pub text: String,
}

/// Whether a block is a generated table-of-contents line.
pub(crate) fn block_is_toc(b: &tilo_doc::snapshot::BlockView) -> bool {
    b.flag("toc")
}

/// The `tab_stops` attr as sorted px positions; malformed entries are skipped.
pub(crate) fn parse_tab_stops(raw: &str) -> Vec<f64> {
    let mut v: Vec<f64> = raw
        .split(',')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        // `parse` takes nan and inf; a stop that is not finite would corrupt the ruler math.
        .filter(|p| p.is_finite() && *p >= 0.0)
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    v
}

pub struct WordProcessor {
    pub(crate) containers: Option<Containers>,
    pub(crate) ids: Option<IdGen>,
    pub(crate) caret: Caret,
    /// The selection anchor: a selection exists while it is set and differs from the caret.
    pub(crate) selection_anchor: Option<Caret>,
    /// Which page region the caret and the edits address: the body, or a page band.
    pub(crate) region: Region,
    /// The `(caret, anchor)` of each region not active, so returning to it restores the caret.
    body_caret: Option<(Caret, Option<Caret>)>,
    header_caret: Option<(Caret, Option<Caret>)>,
    footer_caret: Option<(Caret, Option<Caret>)>,
    edit_core: EditCore,
    /// The document's images, by content address; in memory until the app hands over its store.
    content: tilo_content::ContentStore,
    /// Where every edit goes: checks the participant's permission, attributes the edit and
    /// groups typing into undo steps. A view that was never attached writes nothing.
    dispatch: Option<ui::framework::Dispatcher<crate::framework::Document>>,
    /// The body order while a motion runs, so a block's neighbours cost `O(log n)`, not a listing.
    kept_body: Option<std::rc::Rc<tilo_document::BodyOrder>>,
}

enum Order<'a> {
    Kept(&'a tilo_document::BodyOrder),
    Listed(Vec<String>),
}

impl Order<'_> {
    fn position(&self, id: &str) -> Option<usize> {
        match self {
            Self::Kept(body) => body.position(id),
            Self::Listed(ids) => ids.iter().position(|b| b == id),
        }
    }

    fn id_at(&self, index: usize) -> Option<&str> {
        match self {
            Self::Kept(body) => body.id_at(index),
            Self::Listed(ids) => ids.get(index).map(String::as_str),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Kept(body) => body.len(),
            Self::Listed(ids) => ids.len(),
        }
    }
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
            kept_body: None,
        }
    }

    pub(crate) fn attach(
        &mut self,
        dispatch: ui::framework::Dispatcher<crate::framework::Document>,
    ) {
        self.dispatch = Some(dispatch);
    }

    pub fn set_content_store(&mut self, content: tilo_content::ContentStore) {
        self.content = content;
    }

    pub fn content_store(&self) -> &tilo_content::ContentStore {
        &self.content
    }

    pub fn region(&self) -> Region {
        self.region
    }

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

    /// Switch the edit context to `region`, stashing the caret on the way out and restoring it
    /// on the way back; a header or footer starts with one block. Answers its caret block id.
    pub fn focus_region(&mut self, state: &mut ui::widget::Edits<'_>, region: Region) -> String {
        if region == self.region {
            return self.ensure_region_block(state);
        }
        let prev = (self.caret.clone(), self.selection_anchor.clone());
        let prev_region = self.region;
        *self.caret_slot(prev_region) = Some(prev);

        self.region = region;
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

    pub fn focus_body(&mut self, state: &mut ui::widget::Edits<'_>) -> String {
        self.focus_region(state, Region::Body)
    }

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
            self.collapse_at(id.clone(), 0);
            return id;
        }
        if blocks.contains(&self.caret.block) {
            let len = self.region_block_len(state, &self.caret.block);
            if self.caret.offset > len {
                self.caret.offset = len;
            }
        } else if let Some(first) = blocks.first() {
            self.collapse_at(first.clone(), 0);
        }
        self.caret.block.clone()
    }

    pub fn set_anchor(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.caret.clone());
        }
    }

    fn collapse_at(&mut self, block: String, offset: usize) {
        self.caret = Caret { block, offset };
        self.selection_anchor = None;
    }

    pub fn clear_anchor(&mut self) {
        self.selection_anchor = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection_anchor
            .as_ref()
            .is_some_and(|a| a != &self.caret)
    }

    pub fn selection_span(&self, state: &AutoCommit) -> Option<(Caret, Caret)> {
        // Bail before `ordered_blocks`: a lone caret must not scan the body.
        let anchor = self.selection_anchor.as_ref()?;
        if anchor == &self.caret {
            return None;
        }
        let blocks = self.ordered_blocks(state);
        ordered_selection_span(Some(anchor), &self.caret, |id| {
            blocks.iter().position(|b| b == id)
        })
    }

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

    /// The selection as `(plain text, blocks)`; the plain text is the clipboard key.
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
            let int = |k: &str| b.int_attr(k);
            let bool_ = |k: &str| b.bool_attr(k);
            let str_ = |k: &str| b.str_attr(k).map(str::to_owned);
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

    pub fn stage_replacement(
        &mut self,
        bytes: &[u8],
        format: crate::WpFormat,
    ) -> Option<crate::actions::RichTextAction> {
        crate::import_action(bytes, format, &self.content)
            .map_err(|error| eprintln!("word-processor: could not open the file: {error}"))
            .ok()
    }

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

    /// The document's stylesheet from its CRDT container, or the built-in defaults.
    pub fn stylesheet(&self, state: &AutoCommit) -> StyleSheet {
        let Some(c) = self.containers.as_ref() else {
            return StyleSheet::default();
        };
        let ids = tilo_doc::model::style_ids(state, c);
        if ids.is_empty() {
            return StyleSheet::default();
        }
        // Built-in defaults are the base layer, so a plain block always resolves to a real style;
        // the container's styles overlay them.
        let mut sheet = StyleSheet::default();
        for id in ids {
            let mut props = sheet.get(&id).to_attrs();
            props.extend(tilo_doc::model::style_props(state, c, &id));
            sheet.styles.insert(id, StyleProps::from_attrs(&props));
        }
        sheet
    }

    /// The document's page margins `[top, right, bottom, left]` in px, `None` when it records none.
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

    fn selected_block_ids(&self, state: &AutoCommit) -> Vec<String> {
        let sel = self.selection_blocks(state);
        if sel.is_empty() {
            vec![self.caret.block.clone()]
        } else {
            sel.into_iter().map(|(b, _, _)| b).collect()
        }
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
        dispatch
            .apply_now(state, &crate::model::action(&action))
            .is_ok()
    }

    fn action_region(&self) -> crate::actions::RegionKind {
        match self.region {
            Region::Body => crate::actions::RegionKind::Body,
            Region::Header => crate::actions::RegionKind::Header,
            Region::Footer => crate::actions::RegionKind::Footer,
        }
    }

    /// The selection's ranges, or the whole caret block when there is none.
    fn font_ranges(&self, state: &AutoCommit) -> Vec<(String, usize, usize)> {
        if self.has_selection() {
            self.selection_blocks(state)
        } else {
            let len = self.block_len(state, &self.caret.block);
            vec![(self.caret.block.clone(), 0, len)]
        }
    }

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

    pub fn region_kind(&self) -> crate::actions::RegionKind {
        crate::actions::RegionKind::from_region(self.region)
    }

    pub fn target_block_ids(&self, state: &AutoCommit) -> Vec<String> {
        self.selected_block_ids(state)
    }

    /// A toggle mark's ranges and whether it already covers them; `None` without a selection.
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

    pub fn plan_exclusive_mark(
        &self,
        state: &AutoCommit,
        name: &str,
    ) -> (Vec<(String, usize, usize)>, bool) {
        let ranges = self.font_ranges(state);
        let on = self.mark_over_ranges(state, &ranges, name);
        (ranges, on)
    }

    /// The block's tab stops in px from the left text margin, sorted.
    pub fn block_tab_stops(&self, state: &AutoCommit, block: &str) -> Vec<f64> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        let snap = tilo_doc::model::materialize(state, c);
        let raw = snap
            .blocks
            .iter()
            .find(|b| b.id == block)
            .and_then(|b| b.str_attr("tab_stops").map(str::to_owned))
            .unwrap_or_default();
        parse_tab_stops(&raw)
    }

    pub fn caret_tab_stops(&self, state: &AutoCommit) -> Vec<f64> {
        self.block_tab_stops(state, &self.caret.block.clone())
    }

    fn caret_table(&self, state: &AutoCommit) -> Option<(String, BlockSpans)> {
        let containers = self.containers.as_ref()?;
        let at = |id: &str| {
            tilo_doc::model::materialize_blocks(state, containers, &[id.to_owned()]).pop()
        };
        let cur = at(&self.caret.block)?;
        let tid = cur.str_attr("table")?.to_owned();
        let int = |b: &tilo_doc::snapshot::BlockView, k: &str| {
            b.int_attr(k)
                .filter(|n| *n >= 0)
                .map_or(0, num::usize_of_i64)
        };
        let in_table = |b: &tilo_doc::snapshot::BlockView| {
            b.kind == "cell" && b.str_attr("table") == Some(tid.as_str())
        };
        let order = self.order(state);
        let cells = match (&order, order.position(&self.caret.block)) {
            // A table is a run of consecutive cells: read outward from the caret's cell.
            (Order::Kept(_), Some(pos)) => {
                let mut run = std::collections::VecDeque::from([cur]);
                for i in (0..pos).rev() {
                    match order.id_at(i).and_then(at) {
                        Some(b) if in_table(&b) => run.push_front(b),
                        _ => break,
                    }
                }
                for i in pos + 1..order.len() {
                    match order.id_at(i).and_then(at) {
                        Some(b) if in_table(&b) => run.push_back(b),
                        _ => break,
                    }
                }
                run.iter()
                    .map(|b| (b.id.clone(), int(b, "row"), int(b, "col")))
                    .collect()
            }
            _ => ui::tilo_resource::profiling::measure("word_processor.region_listed", || {
                tilo_doc::model::materialize(state, containers)
            })
            .blocks
            .iter()
            .filter(|b| in_table(b))
            .map(|b| (b.id.clone(), int(b, "row"), int(b, "col")))
            .collect(),
        };
        Some((tid, cells))
    }

    fn caret_rc(&self, cells: &[(String, usize, usize)]) -> (usize, usize) {
        cells
            .iter()
            .find(|(id, _, _)| id == &self.caret.block)
            .map_or((0, 0), |(_, r, c)| (*r, *c))
    }

    /// Move the caret a cell forward or back in row-major order, staying put at each end.
    pub fn tab_cell(&mut self, state: &mut ui::widget::Edits<'_>, forward: bool) -> bool {
        let Some((_, cells)) = self.caret_table(state) else {
            return false;
        };
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
                self.collapse_at(next, 0);
            }
        } else if let Some((prev, ..)) = pos.checked_sub(1).and_then(|i| ordered.get(i).cloned()) {
            // Land at the end of the previous cell's text.
            let off = self.block_len(state, &prev);
            self.collapse_at(prev, off);
        }
        true
    }

    /// Move the caret to the cell above or below, once its text edge is reached.
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
            self.collapse_at(id, off);
            return true;
        }
        false
    }

    pub fn reconcile(&mut self, state: &AutoCommit) {
        // The caret must be in the active region's blocks, or a header caret reads as stray.
        let mut blocks = self.region_blocks_in_order(state);
        if blocks.is_empty() {
            self.region = Region::Body;
            blocks = self.region_blocks_in_order(state);
        }
        if blocks.contains(&self.caret.block) {
            let len = self.block_len(state, &self.caret.block);
            if self.caret.offset > len {
                self.caret.offset = len;
            }
        } else if let Some(first) = blocks.first().cloned() {
            self.collapse_at(first, 0);
        }
    }

    /// Resolve the document, guarantee a block, keep the caret on a real block; writes nothing.
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
        // A body caret proves the body is nonempty: resolve its block, not the whole order.
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
            let Some(c) = crate::model::body_at(state, root)
                .and_then(|body| tilo_doc::model::open(state, &body).ok())
            else {
                return;
            };
            // Seed past existing block ids, so an edit never re-mints one.
            self.ids = Some(tilo_doc::model::seeded_id_gen(state, &c));
            self.containers = Some(c);
        }
        self.reconcile(state);
    }

    fn region_blocks_in_order(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        let _phase = ui::tilo_resource::profiling::Phase::start("word_processor.region_listed");
        tilo_doc::model::region_blocks_in_order(state, c, self.region)
    }

    fn order(&self, state: &AutoCommit) -> Order<'_> {
        match (&self.kept_body, self.region) {
            (Some(body), Region::Body) => Order::Kept(body),
            _ => Order::Listed(self.region_blocks_in_order(state)),
        }
    }

    pub(crate) fn with_kept_body<T>(
        &mut self,
        body: Option<std::rc::Rc<tilo_document::BodyOrder>>,
        motion: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.kept_body = body;
        let out = motion(self);
        self.kept_body = None;
        out
    }

    /// Text of a block in the active region; the body reader hard-codes the body container.
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

    fn block_len(&self, state: &AutoCommit, block: &str) -> usize {
        self.region_block_len(state, block)
    }

    pub fn current_text(&self, state: &AutoCommit) -> String {
        self.region_block_text(state, &self.caret.block)
            .unwrap_or_default()
    }

    pub(crate) fn ordered_blocks(&self, state: &AutoCommit) -> Vec<String> {
        self.region_blocks_in_order(state)
    }

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

    /// The input method's composition at the caret; not in the document until committed.
    pub fn set_preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        self.edit_core.set_preedit(text, cursor);
    }

    pub fn clear_preedit(&mut self) {
        self.edit_core.clear_preedit();
    }

    pub fn preedit(&self) -> &str {
        self.edit_core.preedit()
    }

    pub fn composition(&self, text: &str) -> Option<EditCore> {
        if !self.edit_core.has_preedit() {
            return None;
        }
        let mut core = self.edit_core.clone();
        core.set_caret(crate::text_offsets::char_to_byte(text, self.caret.offset));
        core.clamp_to(text);
        Some(core)
    }

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
        let order = self.order(state);
        if let Some(pos) = order.position(&self.caret.block)
            && pos > 0
            && let Some(prev) = order.id_at(pos - 1).map(str::to_owned)
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
        let order = self.order(state);
        if let Some(pos) = order.position(&self.caret.block)
            && let Some(next) = order.id_at(pos + 1).map(str::to_owned)
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

    pub fn move_block_relative(&mut self, state: &AutoCommit, delta: i32) {
        let order = self.order(state);
        if order.len() == 0 {
            return;
        }
        if let Some(pos) = order.position(&self.caret.block) {
            let new = num::usize_of_i32(
                (num::i32_of_usize(pos) + delta).clamp(0, num::i32_of_usize(order.len()) - 1),
            );
            self.move_to_block(state, new);
        }
    }

    pub fn move_to_block(&mut self, state: &AutoCommit, index: usize) {
        let order = self.order(state);
        if order.len() == 0 {
            return;
        }
        let i = index.min(order.len() - 1);
        let Some(block) = order.id_at(i).map(str::to_owned) else {
            return;
        };
        let len = self.block_len(state, &block);
        let offset = self.caret.offset.min(len);
        self.caret = Caret { block, offset };
    }

    /// The selection when it lies in one block, for the band to paint without layout.
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
    use crate::actions::TableEdit;
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

        wp.clear_anchor();
        wp.enter(&mut Edits::lend(&mut state));
        wp.insert_str(&mut Edits::lend(&mut state), "still editable");
        assert!(
            wp.block_texts(&state)
                .iter()
                .any(|text| text == "still editable")
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn imports_and_binding_keep_the_live_document_and_its_caret() {
        {
            assert_failed_import_preserves_document(crate::WpFormat::Docx);
        }

        {
            assert_failed_import_preserves_document(crate::WpFormat::Odt);
        }

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
                ui::tilo_resource::write_key(
                    &mut state,
                    &ROOT,
                    "host-marker",
                    &ui::tilo_resource::Atom("preserve me".into()),
                )
                .unwrap();
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

        {
            let (wp, state) = bound();
            assert_eq!(wp.block_texts(&state), vec![String::new()]);
            assert_eq!(wp.caret.offset, 0);
            assert!(!wp.caret.block.is_empty());
        }

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
            ui::tilo_resource::TextEdit::Splice {
                at: 0,
                delete: 6,
                insert: "é🦀".into(),
            }
            .apply_under(&mut remote, &text)
            .unwrap();
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

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "text");
            let c = wp.containers.as_ref().unwrap().clone();
            let id = wp.caret.block.clone();
            let (_, block) = state.get(&c.blocks, &id).unwrap().unwrap();
            ui::tilo_resource::Slot::Key(&block, tilo_doc::schema::BLOCK_ORDER)
                .put_scalar(&mut state, automerge::ScalarValue::Bytes(vec![]))
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
                    "scalar_block" => ui::tilo_resource::Slot::Key(&c.blocks, &target)
                        .put_scalar(
                            &mut state,
                            automerge::ScalarValue::Str("not a block".into()),
                        )
                        .unwrap(),
                    "missing_order" => state.delete(&block, tilo_doc::schema::BLOCK_ORDER).unwrap(),
                    _ => ui::tilo_resource::Slot::Key(&block, tilo_doc::schema::BLOCK_ORDER)
                        .put_scalar(&mut state, automerge::ScalarValue::Int(42))
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

    #[test]
    fn stylesheets_resolve_builtins_without_writing_them() {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn caret_and_selection_move_across_blocks() {
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "ab");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "cd");
            wp.move_right(&state);
            assert_eq!(wp.current_text(&state), "cd");
            assert_eq!(wp.caret.offset, 2);
            wp.move_home();
            wp.move_left(&state);
            assert_eq!(wp.current_text(&state), "ab");
            assert_eq!(wp.caret.offset, 2);
        }

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

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
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
        }

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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn marks_and_paragraph_attributes_apply_to_the_selection() {
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
            set_align(&wp, &mut state, "center");
            for b in &blocks {
                assert_eq!(
                    align_of(&wp, &state, b).as_deref(),
                    Some("center"),
                    "paragraph {b} centered"
                );
            }
        }

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
            set_align(&wp, &mut state, "center");
            for c in &cells {
                assert_eq!(
                    align_of(&wp, &state, c).as_deref(),
                    Some("center"),
                    "cell {c} centered"
                );
            }
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello");
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            let c = wp.containers.clone().unwrap();
            let block = wp.caret.block.clone();

            toggle_mark(&wp, &mut state, "strong");
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"),
                "mark applied"
            );
            toggle_mark(&wp, &mut state, "strong");
            assert!(
                !tilo_doc::model::has_mark_over(&state, &c, &block, 0, 5, "strong"),
                "mark toggled off"
            );
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "Title");
            set_align(&wp, &mut state, "center");
            set_paragraph_style(&wp, &mut state, "heading", Some(2));
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn clipboard_copies_keep_structure_and_formatting() {
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "first");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "second");
            wp.enter(&mut Edits::lend(&mut state));
            wp.insert_str(&mut Edits::lend(&mut state), "third");
            let blocks = wp.ordered_blocks(&state);
            assert_eq!(blocks.len(), 3);

            wp.set_caret(Caret {
                block: blocks[0].clone(),
                offset: 2,
            });
            wp.set_anchor();
            wp.set_caret(Caret {
                block: blocks[2].clone(),
                offset: 3,
            });

            assert_eq!(wp.copy_fragment(&state).0, "rst\nsecond\nthi");
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "hello world");
            wp.caret.offset = 0;
            wp.set_anchor();
            wp.caret.offset = 5;
            toggle_mark(&wp, &mut state, "strong");

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

            wp.clear_anchor();
            wp.move_end(&state);
            let at = wp.caret.clone();
            let paste = crate::actions::paste_fragment_action(
                wp.action_region(),
                crate::actions::SelectionTarget {
                    start_block: at.block.clone(),
                    start: at.offset,
                    end_block: at.block,
                    end: at.offset,
                },
                blocks,
                false,
            );
            assert!(wp.apply_rich(&mut Edits::lend(&mut state), paste));
            let c = wp.containers.clone().unwrap();
            let block = wp.caret.block.clone();
            assert_eq!(wp.current_text(&state), "hello worldhello");
            assert!(
                tilo_doc::model::has_mark_over(&state, &c, &block, 11, 16, "strong"),
                "pasted bold preserved"
            );
        }
    }

    fn toggle_mark(wp: &WordProcessor, state: &mut AutoCommit, name: &str) {
        let (spans, active) = wp.plan_toggle_mark(state, name).expect("a selection");
        let action = crate::actions::set_mark_action(
            wp.region_kind(),
            WordProcessor::mark_spans(&spans),
            name,
            (!active).then_some(crate::actions::MarkVal::Bool(true)),
            vec![],
        );
        assert!(wp.apply_rich(&mut Edits::lend(state), action));
    }

    fn set_align(wp: &WordProcessor, state: &mut AutoCommit, align: &str) {
        let action = crate::actions::set_block_attr_action(
            wp.region_kind(),
            wp.target_block_ids(state),
            "align",
            crate::actions::AttrVal::Str(align.into()),
        );
        assert!(wp.apply_rich(&mut Edits::lend(state), action));
    }

    fn set_paragraph_style(
        wp: &WordProcessor,
        state: &mut AutoCommit,
        kind: &str,
        level: Option<i64>,
    ) {
        let action = crate::actions::set_paragraph_style_action(
            wp.region_kind(),
            wp.paragraph_style_targets(state),
            kind,
            level,
            None,
        );
        assert!(wp.apply_rich(&mut Edits::lend(state), action));
    }

    fn align_of(wp: &WordProcessor, state: &AutoCommit, id: &str) -> Option<String> {
        let c = wp.containers.as_ref().unwrap();
        tilo_doc::model::materialize(state, c)
            .blocks
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| b.str_attr("align").map(str::to_owned))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn tables_stay_rectangular_and_editable() {
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
            edit_caret_table(&wp, &mut state, TableEdit::InsertRow { at: 1 });
            assert_eq!(count(&wp, &state), 6);
            edit_caret_table(&wp, &mut state, TableEdit::InsertColumn { at: 1 });
            assert_eq!(count(&wp, &state), 9);
            edit_caret_table(&wp, &mut state, TableEdit::DeleteRow { row: 1 });
            assert_eq!(count(&wp, &state), 6);
            edit_caret_table(&wp, &mut state, TableEdit::DeleteColumn { column: 1 });
            assert_eq!(count(&wp, &state), 4);
            edit_caret_table(&wp, &mut state, TableEdit::Delete);
            assert_eq!(count(&wp, &state), 0);
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "before");
            wp.insert_table(&mut Edits::lend(&mut state), 2, 3);
            let snap = tilo_doc::model::materialize(&state, wp.containers.as_ref().unwrap());
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(cells.len(), 6, "2x3 = 6 cells");
            let tid = match cells[0].attrs.get("table") {
                Some(tilo_doc::AttrVal::Str(s)) => s.clone(),
                other => panic!("cell missing table attr: {other:?}"),
            };
            assert!(
                cells
                    .iter()
                    .all(|c| c.attrs.get("table") == Some(&tilo_doc::AttrVal::Str(tid.clone())))
            );
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

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 3);
            assert_rectangular(&table_grid(&wp, &state), 2, 3);
            edit_caret_table(&wp, &mut state, TableEdit::InsertRow { at: 1 });
            let grid = table_grid(&wp, &state);
            let rows = grid.iter().map(|(r, _)| *r).max().unwrap() + 1;
            assert_eq!(rows, 3, "row count increased by one");
            assert_rectangular(&grid, 3, 3);
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            edit_caret_table(&wp, &mut state, TableEdit::InsertRow { at: 1 });
            assert_rectangular(&table_grid(&wp, &state), 3, 2);
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 3, 3);
            let before = table_grid(&wp, &state).len();
            assert_eq!(before, 9);
            edit_caret_table(&wp, &mut state, TableEdit::DeleteColumn { column: 0 });
            let grid = table_grid(&wp, &state);
            assert_eq!(grid.len(), before - 3, "exactly one cell removed per row");
            assert_rectangular(&grid, 3, 2);
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
            edit_caret_table(&wp, &mut state, TableEdit::InsertColumn { at: 1 });
            assert_rectangular(&table_grid(&wp, &state), 2, 3);
            edit_caret_table(&wp, &mut state, TableEdit::InsertColumn { at: 3 });
            assert_rectangular(&table_grid(&wp, &state), 2, 4);
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 1, 2);
            edit_caret_table(&wp, &mut state, TableEdit::DeleteRow { row: 0 });
            let c = wp.containers.as_ref().unwrap();
            let snap = tilo_doc::model::materialize(&state, c);
            assert!(
                snap.blocks.iter().all(|b| b.kind != "cell"),
                "deleting the last row removed the whole table"
            );
        }
    }

    fn edit_caret_table(wp: &WordProcessor, state: &mut AutoCommit, edit: TableEdit) {
        let (table, _) = wp.caret_table(state).expect("the caret is in a table");
        let action = crate::actions::edit_table_action(&table, edit);
        assert!(
            wp.apply_rich(&mut Edits::lend(state), action),
            "the table edit applies"
        );
    }

    fn table_grid(wp: &WordProcessor, state: &AutoCommit) -> Vec<(usize, usize)> {
        let c = wp.containers.as_ref().unwrap();
        let snap = tilo_doc::model::materialize(state, c);
        let int = |b: &tilo_doc::snapshot::BlockView, k: &str| {
            b.int_attr(k)
                .filter(|n| *n >= 0)
                .map_or(0, num::usize_of_i64)
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

    #[test]
    fn table_navigation_walks_cells_and_leaves_structure_to_the_host() {
        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2); // caret in (0,0)
            for expect in [(0, 1), (1, 0), (1, 1)] {
                assert!(
                    wp.tab_cell(&mut Edits::lend(&mut state), true),
                    "tab handled inside the table"
                );
                assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), expect);
            }
            assert!(wp.tab_cell(&mut Edits::lend(&mut state), true));
            let grid = table_grid(&wp, &state);
            assert_rectangular(&grid, 2, 2);
            assert_eq!(
                wp.caret_rc(&wp.caret_table(&state).unwrap().1),
                (1, 1),
                "caret stays at the stable edge target for the action host"
            );
        }

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2);
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

        {
            let (mut wp, mut state) = bound();
            wp.insert_table(&mut Edits::lend(&mut state), 2, 2); // caret in (0,0)
            assert!(wp.cell_vertical(&state, true), "down moved into a cell");
            assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), (1, 0));
            assert!(wp.cell_vertical(&state, false), "up moved back");
            assert_eq!(wp.caret_rc(&wp.caret_table(&state).unwrap().1), (0, 0));
            assert!(
                !wp.cell_vertical(&state, false),
                "no cell above the top row"
            );
        }
    }

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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn header_and_footer_regions_edit_independently_of_the_body() {
        {
            let (mut wp, mut state) = bound();
            wp.insert_str(&mut Edits::lend(&mut state), "body-content");
            assert_eq!(wp.region(), Region::Body);

            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            assert_eq!(wp.region(), Region::Header);
            assert!(wp.in_running_region());
            wp.insert_str(&mut Edits::lend(&mut state), "My Title");

            assert_eq!(header_texts(&wp, &state), vec!["My Title".to_string()]);
            assert_eq!(body_texts(&wp, &state), vec!["body-content".to_string()]);
        }

        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "Head");
            wp.focus_body(&mut Edits::lend(&mut state));
            assert_eq!(wp.region(), Region::Body);
            wp.insert_str(&mut Edits::lend(&mut state), "Body");
            assert_eq!(body_texts(&wp, &state), vec!["Body".to_string()]);
            assert_eq!(header_texts(&wp, &state), vec!["Head".to_string()]);
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "er");
            assert_eq!(header_texts(&wp, &state), vec!["Header".to_string()]);
        }

        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Header);
            wp.insert_str(&mut Edits::lend(&mut state), "Title");
            wp.select_all(&state);
            toggle_mark(&wp, &mut state, "strong");

            let c = wp.containers.as_ref().unwrap();
            let hv = tilo_doc::model::materialize_region(&state, c, Region::Header);
            let marks = &hv[0].marks;
            assert!(
                marks
                    .iter()
                    .any(|m| m.name == "strong" && m.start == 0 && m.end >= 5),
                "bold mark applied to the header run: {marks:?}"
            );
            wp.select_all(&state);
            toggle_mark(&wp, &mut state, "strong");
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

        {
            let (mut wp, mut state) = bound();
            wp.focus_region(&mut Edits::lend(&mut state), Region::Footer);
            wp.insert_str(&mut Edits::lend(&mut state), "Page ");
            let at = wp.caret.clone();
            let field =
                crate::actions::insert_page_number_action(wp.action_region(), &at.block, at.offset);
            assert!(wp.apply_rich(&mut Edits::lend(&mut state), field));

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

        {
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
            assert!(header_texts(&wp, &state).is_empty());
            assert!(footer_texts(&wp, &state).is_empty());
        }
    }
}
