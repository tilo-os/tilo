//! Direct edits on a `WordProcessor` without a widget, for tests that need a
//! document in a given shape. Each one builds the same typed action the editor
//! dispatches and applies it through the model's dispatcher; the editor itself
//! edits through the widget's action path, never through these.

use super::{BlockSpans, Caret, WordProcessor};
use automerge::AutoCommit;
use tilo_doc::model::Region;
use ui::text_edit::Edit;

impl WordProcessor {
    /// Replace the whole body with `bytes` decoded as `format`: the staged
    /// import action Open submits, then the caret seated at the start. A
    /// decode failure leaves the document and caret intact.
    pub(crate) fn replace_with_format(
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

    /// The selected text, joining blocks with newlines.
    pub(crate) fn selection_text(&self, state: &AutoCommit) -> String {
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

    /// Delete the current selection (no-op if none); returns whether it deleted.
    pub(crate) fn delete_selection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
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

    /// The blocks a paragraph-style change targets — the selection (or caret
    /// block), minus table cells (a cell keeps its `cell` kind).
    pub(crate) fn paragraph_style_targets(&self, state: &AutoCommit) -> Vec<String> {
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
    pub(crate) fn format_spans(&self, state: &AutoCommit) -> BlockSpans {
        self.font_ranges(state)
    }

    /// Insert a `rows`×`cols` table after the caret block. Cells are modeled as
    /// consecutive top-level `cell` blocks tagged with `table`/`row`/`col`, so
    /// they edit/select/collaborate through the normal block machinery; the
    /// widget renders consecutive same-`table` cells as a grid.
    pub(crate) fn insert_table(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        rows: usize,
        cols: usize,
    ) {
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
        self.collapse_at(first.clone(), 0);
    }

    /// Insert an image block carrying `bytes` at the caret; the bytes go in the
    /// app's content store and the block's `blob` attr references them
    /// (mirroring `tilo_doc_codecs::odt`). Mid-text, the block splits so the image
    /// lands between the text before/after the caret; else it inserts after the
    /// caret block. The caret lands in a fresh empty paragraph just after the image.
    ///
    /// Decodable bytes record intrinsic pixel `width`/`height` as attrs (matching
    /// ODF import); undecodable bytes still produce a valid block (a placeholder).
    pub(crate) fn insert_image_bytes(&mut self, state: &mut ui::widget::Edits<'_>, bytes: &[u8]) {
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

    /// Ordered block texts (empty blocks render as empty strings).
    pub(crate) fn block_texts(&self, state: &AutoCommit) -> Vec<String> {
        let Some(c) = self.containers.as_ref() else {
            return vec![];
        };
        tilo_doc::model::blocks_in_order(state, c)
            .iter()
            .map(|id| tilo_doc::model::block_text(state, c, id).unwrap_or_default())
            .collect()
    }

    /// Select the entire document (Ctrl+A).
    pub(crate) fn select_all(&mut self, state: &AutoCommit) {
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

    /// Insert `s` at the caret and advance. Replaces the selection if any.
    /// Routes through the active region (body / header / footer).
    pub(crate) fn insert_str(&mut self, state: &mut ui::widget::Edits<'_>, s: &str) {
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
    pub(crate) fn enter(&mut self, state: &mut ui::widget::Edits<'_>) {
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

    pub(super) fn mark_spans(ranges: &[(String, usize, usize)]) -> Vec<crate::actions::Span> {
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

    pub(super) fn region_splice(
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

    pub(super) fn region_set_kind(
        &self,
        state: &mut ui::widget::Edits<'_>,
        block: &str,
        kind: &str,
    ) {
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

    /// `(kind, ordered, level)` for a block in the **active region**.
    pub(super) fn block_meta(
        &self,
        state: &AutoCommit,
        block: &str,
    ) -> (String, bool, Option<i64>) {
        let Some(c) = self.containers.as_ref() else {
            return ("paragraph".into(), false, None);
        };
        let views = match self.region {
            Region::Body => tilo_doc::model::materialize(state, c).blocks,
            r => tilo_doc::model::materialize_region(state, c, r),
        };
        let b = views.iter().find(|b| b.id == block);
        let kind = b.map_or_else(|| "paragraph".into(), |b| b.kind.clone());
        let ordered = b.is_some_and(|b| b.flag("ordered"));
        let level = b.and_then(|b| b.int_attr("level"));
        (kind, ordered, level)
    }

    pub(super) fn apply_edit_core(
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
}
