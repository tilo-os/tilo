//! Text selection over the PDF's own text layer, in its reading order: a
//! drag across lines and pages (scrolling at the canvas edge), double- and
//! triple-click for a word and a line, Shift+click to extend, select all,
//! copy; on a touchscreen a hold selects a word and grips move either end.
//!
//! A page's text layer comes from the PDF worker. It is asked for ahead of
//! the press, while the pointer rests on the page, and a press that lands
//! before it arrives waits for it: it becomes the selection (or, over no
//! text, the pan) it would have been.

use std::sync::Arc;

use ui::gesture::Granularity;
use vello::kurbo::{Point, Rect};

use super::{PanState, PdfApp};
use crate::model::{AnnoKind, Annotation, PtRect, Swatch};
use crate::source::{TextBatch, TextRequest};
use crate::text_layer::PageText;

/// Text layers the reader keeps beyond those on screen, selected or found.
const MAX_TEXT_LAYERS: usize = 48;

/// A place in the document's text: a displayed page (1-based) and a caret
/// position of its text layer, before glyph `glyph` (`glyph` = the layer's
/// length is after its last). Ordered as the text reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TextPos {
    pub(super) page: usize,
    pub(super) glyph: usize,
}

impl TextPos {
    pub(super) fn new(page: usize, glyph: usize) -> Self {
        Self { page, glyph }
    }
}

/// A text selection between two caret positions: the anchor is where it
/// began, the caret follows the pointer.
#[derive(Clone, Copy, Debug)]
pub(super) struct TextSelection {
    pub(super) anchor: TextPos,
    pub(super) caret: TextPos,
    /// True while a press or grip is extending it.
    pub(super) dragging: bool,
}

impl TextSelection {
    pub(super) fn start(&self) -> TextPos {
        self.anchor.min(self.caret)
    }

    pub(super) fn end(&self) -> TextPos {
        self.anchor.max(self.caret)
    }

    /// Whether it covers any text (a bare click selects nothing).
    pub(super) fn is_active(&self) -> bool {
        self.anchor != self.caret
    }

    /// The pages it spans.
    pub(super) fn pages(&self) -> std::ops::RangeInclusive<usize> {
        self.start().page..=self.end().page
    }

    /// The glyphs it covers on `page`, whose layer holds `len`, as `[lo, hi)`.
    pub(super) fn range_on(&self, page: usize, len: usize) -> Option<(usize, usize)> {
        let (start, end) = (self.start(), self.end());
        if page < start.page || page > end.page {
            return None;
        }
        let lo = if page == start.page { start.glyph } else { 0 };
        let hi = if page == end.page { end.glyph } else { len }.min(len);
        (lo < hi).then_some((lo, hi))
    }
}

/// A press on a page whose text layer had not arrived yet.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingPress {
    pub(super) page: usize,
    pub(super) pt: (f64, f64),
    pub(super) press: Point,
    pub(super) scroll: (f64, f64),
    /// Where the button lifted, when the drag ended before the text came:
    /// the gesture is carried out whole once it does.
    pub(super) released: Option<Point>,
}

/// What a press on page text began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionStart {
    /// A selection, at the glyph pressed.
    Armed,
    /// Nothing: there is no text there.
    NoText,
    /// The page's text is on its way; the press waits for it.
    Waiting,
}

/// A selection effect an agent or assistive tool invoked, waiting for the
/// text of the pages it reads.
pub(super) struct PendingSelectionEffect {
    pub(super) effect: SelectionEffect,
    /// The displayed pages (1-based) whose text it needs.
    pub(super) pages: Vec<usize>,
    pub(super) reply: ui::framework::EffectReply,
}

/// What a selection effect does once its pages' text is here.
#[derive(Clone, Debug)]
pub(super) enum SelectionEffect {
    /// Select `text` (all of the page's text when `None`) on `page`.
    Select { text: Option<String>, page: usize },
    /// Copy the live selection.
    Copy,
}

impl PdfApp {
    /// The text layer of displayed page `page`, asked of the worker ahead
    /// of anything read ahead when it is not here yet.
    pub(super) fn ensure_raster_text(&mut self, page: usize) -> Option<Arc<PageText>> {
        if let Some(layer) = self.raster_text.get(&page) {
            return Some(layer.clone());
        }
        if let Some(document) = self.raster_document.as_ref()
            && page > 0
            && self.raster_text_pending.insert(page - 1)
        {
            if !self.raster_handle.as_ref().is_some_and(|handle| {
                handle
                    .request_text(TextRequest {
                        document_id: document.id,
                        page: page - 1,
                    })
                    .is_ok()
            }) {
                self.raster_text_pending.remove(&(page - 1));
                self.status_note.show(crate::t("pdf-note-text-unavailable"));
            }
            self.dirty = true;
        }
        None
    }

    /// The displayed page `p` (screen) is on, or the nearest one above or
    /// below it.
    pub(super) fn page_near(&self, p: Point) -> Option<usize> {
        let count = self.doc.page_count();
        let unit = self.stack_unit();
        if count == 0 || unit <= 0.0 {
            return None;
        }
        let y = self.viewport.screen_to_content(p).y;
        let index = (y / unit).floor().clamp(0.0, (count - 1) as f64) as usize;
        Some(index + 1)
    }

    /// `p` (screen) in page points of displayed page `page`, past its edges
    /// as well.
    fn point_on_page(&self, page: usize, p: Point) -> (f64, f64) {
        let origin = self.viewport.content_point_to_screen(Point::new(
            0.0,
            page.saturating_sub(1) as f64 * self.stack_unit(),
        ));
        let zoom = self.zoom().max(1e-9);
        ((p.x - origin.x) / zoom, (p.y - origin.y) / zoom)
    }

    /// The caret position `p` (screen) falls at: on the page under it (or
    /// the nearest), before or after the glyph under it (or the nearest) by
    /// which half of it `p` is over. `None` while that page's text is on
    /// its way.
    fn text_pos_at(&mut self, p: Point) -> Option<TextPos> {
        let page = self.page_near(p)?;
        let layer = self.ensure_raster_text(page)?;
        let (x, y) = self.point_on_page(page, p);
        let glyph = layer.char_at(x, y).or_else(|| layer.nearest_char(x, y))?;
        Some(TextPos::new(page, caret_by(&layer, glyph, x)))
    }

    /// Begin a text selection at page-point `pt` on `page`, through the
    /// shared [`ui::gesture::DragSelect`] click ladder: a click places the
    /// caret, a double-click selects the word, a triple-click the line, as
    /// in every text field and the ebook reader. `press` is the screen
    /// point, for the multi-click distance.
    pub(super) fn begin_selection(
        &mut self,
        page: usize,
        pt: (f64, f64),
        press: Point,
    ) -> SelectionStart {
        let Some(layer) = self.ensure_raster_text(page) else {
            return if self.raster_text_pending.contains(&page.saturating_sub(1)) {
                SelectionStart::Waiting
            } else {
                SelectionStart::NoText
            };
        };
        let Some(glyph) = layer.char_at(pt.0, pt.1) else {
            return SelectionStart::NoText;
        };
        let intent = self.drag_select.press(press, false);
        self.arm_selection(page, glyph, pt.0, intent.granularity, &layer);
        SelectionStart::Armed
    }

    /// Start a selection over glyph `glyph` of `page`, pressed at page x
    /// `x`, snapped to `granularity`'s unit.
    fn arm_selection(
        &mut self,
        page: usize,
        glyph: usize,
        x: f64,
        granularity: Granularity,
        layer: &PageText,
    ) {
        let unit = match granularity {
            Granularity::Word => Some(layer.word_range(glyph)),
            Granularity::Line => Some(layer.row_range(glyph)),
            Granularity::Char => None,
        }
        .filter(|(s, e)| e > s);
        if let Some((s, e)) = unit {
            let (s, e) = (TextPos::new(page, s), TextPos::new(page, e));
            self.selection = Some(TextSelection {
                anchor: s,
                caret: e,
                dragging: true,
            });
            self.sel_gran = granularity;
            self.sel_anchor_range = (s, e);
        } else {
            let at = TextPos::new(page, caret_by(layer, glyph, x));
            self.selection = Some(TextSelection {
                anchor: at,
                caret: at,
                dragging: true,
            });
            self.sel_gran = Granularity::Char;
            self.sel_anchor_range = (at, at);
        }
        self.dirty = true;
    }

    /// A press that waited for its page's text now has it (or the pointer
    /// moved): it becomes the selection it would have begun, extended to
    /// `now`, or over no text the pan it would have been.
    pub(super) fn resolve_pending_press(&mut self, now: Point) {
        let Some(pending) = self.pending_press else {
            return;
        };
        let Some(layer) = self.raster_text.get(&pending.page).cloned() else {
            return;
        };
        self.pending_press = None;
        let now = pending.released.unwrap_or(now);
        if let Some(glyph) = layer.char_at(pending.pt.0, pending.pt.1) {
            let intent = self.drag_select.press(pending.press, false);
            self.arm_selection(
                pending.page,
                glyph,
                pending.pt.0,
                intent.granularity,
                &layer,
            );
            if now != pending.press {
                self.update_selection_at(now);
            }
            if pending.released.is_some() {
                self.finish_selection();
            }
        } else {
            self.pan = Some(PanState {
                start: pending.press,
                scroll_x0: pending.scroll.0,
                scroll_y0: pending.scroll.1,
                moved: false,
            });
            self.on_cursor_moved_page_drags(now);
            if pending.released.is_some() {
                self.pan = None;
            }
        }
    }

    /// The button lifted while a press waited for its page's text. A press
    /// that never moved is a click, there and then; a drag is carried out
    /// when the text comes. Whether it was a click.
    pub(super) fn release_pending_press(&mut self, at: Point) -> Option<PendingPress> {
        let pending = self.pending_press?;
        if pending.press.distance(at) <= super::PAN_SLOP {
            self.pending_press = None;
            return Some(pending);
        }
        if let Some(waiting) = self.pending_press.as_mut() {
            waiting.released = Some(at);
        }
        None
    }

    /// Shift+press: move the live selection's caret to `p` and keep
    /// extending it while the button is held. Whether there was a
    /// selection to extend.
    pub(super) fn extend_selection_to(&mut self, p: Point) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        let _ = self.drag_select.press(p, true);
        self.sel_gran = Granularity::Char;
        let caret = self.text_pos_at(p).unwrap_or(sel.caret);
        self.selection = Some(TextSelection {
            anchor: sel.anchor,
            caret,
            dragging: true,
        });
        self.dirty = true;
        true
    }

    /// Extend the live selection to the pointer at `p` (screen), on
    /// whichever page it is over. A character drag walks the caret glyph by
    /// glyph; after a double- or triple-click it keeps the anchored unit and
    /// grows by whole units ([`ui::extend_span_drag`]).
    pub(super) fn update_selection_at(&mut self, p: Point) {
        let Some(sel) = self.selection.filter(|sel| sel.dragging) else {
            return;
        };
        let Some(pos) = self.text_pos_at(p) else {
            return;
        };
        let (anchor, caret) = match self.sel_gran {
            Granularity::Char => (sel.anchor, pos),
            Granularity::Word | Granularity::Line => {
                let Some(layer) = self.raster_text.get(&pos.page) else {
                    return;
                };
                // The glyph the pointer is over, not the caret beside it.
                let glyph = pos.glyph.min(layer.chars.len().saturating_sub(1));
                let (lo, hi) = if self.sel_gran == Granularity::Word {
                    layer.word_range(glyph)
                } else {
                    layer.row_range(glyph)
                };
                let unit_lo = TextPos::new(pos.page, lo);
                let unit_hi = TextPos::new(pos.page, hi.max(lo + 1));
                let (a_lo, a_hi) = self.sel_anchor_range;
                let at = TextPos::new(pos.page, glyph);
                let (a, c) = ui::extend_span_drag(a_lo, a_hi, at, unit_lo, unit_hi);
                (a.min(c), a.max(c))
            }
        };
        if let Some(s) = self.selection.as_mut() {
            s.anchor = anchor;
            s.caret = caret;
        }
        self.dirty = true;
    }

    /// End the drag. A selection that never grew past its anchor (a bare
    /// click) is dropped, so a click leaves no one-glyph highlight.
    pub(super) fn finish_selection(&mut self) {
        self.drag_select.release();
        self.selection_autoscroll.stop();
        if let Some(s) = self.selection.as_mut() {
            s.dragging = false;
            if !s.is_active() {
                self.selection = None;
            }
        }
        self.dirty = true;
    }

    pub(super) fn clear_selection(&mut self) {
        self.pending_copy = false;
        if self.selection.take().is_some() {
            self.dirty = true;
        }
    }

    /// Select all the document's text. Whether there is a document whose
    /// text can be selected.
    pub(super) fn select_all(&mut self) -> bool {
        let count = self.doc.page_count();
        if !self.raster_active() || count == 0 {
            return false;
        }
        self.selection = Some(TextSelection {
            anchor: TextPos::new(1, 0),
            caret: TextPos::new(count, usize::MAX),
            dragging: false,
        });
        self.sel_gran = Granularity::Char;
        self.submit_text_batch();
        self.dirty = true;
        true
    }

    /// Answer a selection effect: select text on a page, or copy the
    /// selection. Answered at once when the pages' text is here, else when
    /// it arrives (or the document goes).
    ///
    /// # Errors
    /// Returns why the effect cannot run.
    pub(crate) fn selection_effect(
        &mut self,
        effect: &crate::actions::PdfEffect,
        reply: impl FnOnce() -> ui::framework::EffectReply,
    ) -> ui::framework::EffectAnswer {
        use crate::actions::PdfEffect;
        if !self.raster_active() {
            return Err("no PDF is open".into());
        }
        if self.pending_selection_effect.is_some() {
            return Err("another selection is waiting for its text".into());
        }
        let (effect, pages) = match effect {
            PdfEffect::SelectText { text, page } => {
                let count = self.doc.page_count();
                let page = page.unwrap_or(self.current_page);
                if page == 0 || page > count {
                    return Err(format!("the PDF has {count} pages"));
                }
                let text = text.clone().filter(|text| !text.trim().is_empty());
                (SelectionEffect::Select { text, page }, vec![page])
            }
            PdfEffect::CopySelection => {
                let Some(sel) = self.selection.filter(TextSelection::is_active) else {
                    return Err("no text is selected".into());
                };
                (SelectionEffect::Copy, sel.pages().collect())
            }
            _ => return Err("not a selection effect".into()),
        };
        if pages.iter().all(|page| self.raster_text.contains_key(page)) {
            return self
                .carry_out_selection_effect(&effect)
                .map(ui::framework::Effected::value);
        }
        self.pending_selection_effect = Some(PendingSelectionEffect {
            effect,
            pages,
            reply: reply(),
        });
        self.submit_text_batch();
        Ok(ui::framework::Effected::Pending)
    }

    /// A waiting selection effect whose pages' text has all arrived is
    /// carried out and answered.
    fn answer_pending_selection_effect(&mut self) {
        let ready = self
            .pending_selection_effect
            .as_ref()
            .is_some_and(|pending| {
                pending
                    .pages
                    .iter()
                    .all(|page| self.raster_text.contains_key(page))
            });
        if !ready {
            return;
        }
        let Some(pending) = self.pending_selection_effect.take() else {
            return;
        };
        let answer = self
            .carry_out_selection_effect(&pending.effect)
            .map(|value| ui::tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            });
        pending.reply.finish(answer);
    }

    /// A waiting selection effect fails: its document went, or its text
    /// cannot be read.
    pub(super) fn fail_pending_selection_effect(&mut self, why: &str) {
        self.pending_copy = false;
        self.pending_press = None;
        if let Some(pending) = self.pending_selection_effect.take() {
            pending.reply.finish(Err(why.to_owned()));
        }
    }

    /// Do `effect`, every page's text being here: answers with the selected
    /// (or copied) text.
    fn carry_out_selection_effect(
        &mut self,
        effect: &SelectionEffect,
    ) -> Result<serde_json::Value, String> {
        match effect {
            SelectionEffect::Select { text, page } => {
                let layer = self
                    .raster_text
                    .get(page)
                    .cloned()
                    .ok_or_else(|| format!("page {page} has no text"))?;
                let (start, end) = match text {
                    Some(text) => {
                        let hit = layer
                            .search(*page, text, crate::model::SearchOptions::default())
                            .into_iter()
                            .next()
                            .ok_or_else(|| format!("{text:?} is not on page {page}"))?;
                        (hit.char_start, hit.char_start + hit.char_len)
                    }
                    None => (0, layer.chars.len()),
                };
                if end <= start {
                    return Err(format!("page {page} has no text"));
                }
                self.selection = Some(TextSelection {
                    anchor: TextPos::new(*page, start),
                    caret: TextPos::new(*page, end),
                    dragging: false,
                });
                self.sel_gran = Granularity::Char;
                self.go_to_page(*page);
                self.dirty = true;
                Ok(serde_json::json!({
                    "page": page,
                    "text": layer.text_for_range(start, end),
                }))
            }
            SelectionEffect::Copy => {
                let text = self
                    .selection_text()
                    .ok_or_else(|| "the selection holds no text".to_owned())?;
                self.copy_selection();
                Ok(serde_json::json!({ "text": text }))
            }
        }
    }

    /// The pages the live selection spans whose text is not here yet.
    pub(super) fn selection_pages_missing(&self) -> Vec<usize> {
        self.selection
            .filter(TextSelection::is_active)
            .map(|sel| {
                sel.pages()
                    .filter(|page| !self.raster_text.contains_key(page))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The selected text, pages joined by a line break, once every page it
    /// spans has its text here.
    pub(super) fn selection_text(&self) -> Option<String> {
        let sel = self.selection.filter(TextSelection::is_active)?;
        let mut parts = Vec::new();
        for page in sel.pages() {
            let layer = self.raster_text.get(&page)?;
            if let Some((a, b)) = sel.range_on(page, layer.chars.len()) {
                parts.push(layer.text_for_range(a, b));
            }
        }
        let text = parts.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// Copy the selection to the clipboard and say so. A selection over
    /// pages whose text is still on its way is copied when it arrives.
    /// Whether there was a selection to copy.
    pub(super) fn copy_selection(&mut self) -> bool {
        if !self.selection.is_some_and(|sel| sel.is_active()) {
            return false;
        }
        if !self.selection_pages_missing().is_empty() {
            self.pending_copy = true;
            self.status_note.show_progress(crate::t("pdf-note-copying"));
            self.submit_text_batch();
            self.dirty = true;
            return true;
        }
        self.pending_copy = false;
        let Some(text) = self.selection_text() else {
            return false;
        };
        ui::clipboard_copy(&text);
        let n = text.chars().count();
        self.status_note.show(crate::t_args(
            "pdf-note-copied-text",
            &[("count", n.into())],
        ));
        self.dirty = true;
        true
    }

    /// A page's text arrived: a press waiting for it resolves, a copy
    /// waiting for it completes, and the reader keeps its layers bounded.
    pub(super) fn text_arrived(&mut self, page: usize) {
        if self
            .pending_press
            .is_some_and(|pending| pending.page == page)
        {
            self.resolve_pending_press(self.last_mouse);
            self.dirty = true;
        } else if self.selection.is_some_and(|sel| sel.dragging) {
            // A drag onto a page whose text was on its way reaches it now.
            self.update_selection_at(self.last_mouse);
            self.dirty = true;
        }
        if self.pending_copy && self.selection_pages_missing().is_empty() {
            self.copy_selection();
        }
        self.answer_pending_selection_effect();
        self.evict_text_layers();
        self.submit_text_batch();
    }

    /// A page's text cannot be read: a press waiting for it pans instead,
    /// and a copy or effect that needs it fails.
    pub(super) fn text_failed(&mut self, page: usize, why: &str) {
        if self
            .pending_press
            .is_some_and(|pending| pending.page == page)
            && let Some(pending) = self.pending_press.take()
        {
            self.begin_pan(pending.press);
        }
        if self.pending_copy
            && self
                .selection
                .is_some_and(|sel| sel.pages().contains(&page))
        {
            self.pending_copy = false;
        }
        if self
            .pending_selection_effect
            .as_ref()
            .is_some_and(|pending| pending.pages.contains(&page))
            && let Some(pending) = self.pending_selection_effect.take()
        {
            pending.reply.finish(Err(why.to_owned()));
        }
    }

    /// The worker dropped the text it was asked for: ask again for what a
    /// waiting press, copy or effect needs.
    pub(super) fn text_requests_dropped(&mut self) {
        if let Some(pending) = self.pending_press {
            self.ensure_raster_text(pending.page);
        }
        self.submit_text_batch();
    }

    /// Every text layer is gone (another document, its pages rearranged):
    /// the selection, which named places in them, goes too, and whatever
    /// waited for them fails.
    pub(super) fn text_layers_replaced(&mut self, why: &str) {
        self.selection = None;
        self.text_hover = None;
        self.raster_text_batch.clear();
        self.fail_pending_selection_effect(why);
    }

    /// The pages were laid out again (the shared page plan arrived or
    /// changed): every text layer is gone and the selection with it, so a
    /// copy of it fails. A selection effect names pages as the reader shows
    /// them, so it waits for their text in the new layout instead.
    pub(super) fn page_plan_replaced(&mut self, why: &str) {
        let selecting = self
            .pending_selection_effect
            .take_if(|pending| matches!(pending.effect, SelectionEffect::Select { .. }));
        self.text_layers_replaced(why);
        self.pending_selection_effect = selecting;
    }

    /// Drop text layers beyond [`MAX_TEXT_LAYERS`] that nothing shows or
    /// needs (on screen, selected, found, under the pointer), farthest from
    /// the current page first.
    fn evict_text_layers(&mut self) {
        if self.raster_text.len() <= MAX_TEXT_LAYERS
            || self.finding()
            || !self.raster_matches.is_empty()
        {
            return;
        }
        let visible = self.visible_page_range();
        let selected = self.selection.map(|sel| sel.pages());
        let current = self.current_page;
        let mut spare: Vec<usize> = self
            .raster_text
            .keys()
            .copied()
            .filter(|page| {
                !visible.contains(page)
                    && !selected.as_ref().is_some_and(|pages| pages.contains(page))
                    && self.text_hover != Some(*page)
            })
            .collect();
        spare.sort_by_key(|page| std::cmp::Reverse(page.abs_diff(current)));
        let excess = self.raster_text.len() - MAX_TEXT_LAYERS;
        for page in spare.into_iter().take(excess) {
            self.raster_text.remove(&page);
        }
    }

    /// The displayed pages (1-based) the canvas shows.
    pub(super) fn visible_page_range(&self) -> std::ops::Range<usize> {
        let canvas = self.canvas_rect;
        match (
            self.page_near(Point::new(canvas.x0, canvas.y0)),
            self.page_near(Point::new(canvas.x0, canvas.y1)),
        ) {
            (Some(first), Some(last)) => first..last + 1,
            _ => 0..0,
        }
    }

    /// The pointer rests at `p`: the page under it is about to be pressed,
    /// so its text is read ahead (after every pixel and asked text).
    pub(super) fn hover_text(&mut self, p: Point) {
        let hover = if self.raster_active()
            && matches!(
                self.tool,
                crate::model::Tool::Select | crate::model::Tool::Highlight
            ) {
            self.hit_page(p).map(|(page, _)| page)
        } else {
            None
        };
        if hover != self.text_hover {
            self.text_hover = hover;
            self.submit_text_batch();
        }
    }

    /// Hand the worker the text layers wanted ahead of need: the page under
    /// the pointer, and every page of a selection being copied or answered
    /// to a caller. Replaces the previous batch; empty cancels it.
    pub(super) fn submit_text_batch(&mut self) {
        let Some(document_id) = self.raster_document.as_ref().map(|document| document.id) else {
            return;
        };
        let mut pages = Vec::new();
        pages.extend(self.text_hover);
        if self.pending_copy || self.pending_selection_effect.is_some() {
            pages.extend(self.selection_pages_missing());
        }
        if let Some(effect) = self.pending_selection_effect.as_ref() {
            pages.extend(effect.pages.clone());
        }
        let batch: Vec<usize> = pages
            .into_iter()
            .filter(|page| *page > 0 && !self.raster_text.contains_key(page))
            .map(|page| page - 1)
            .filter(|page| !self.raster_text_pending.contains(page))
            .collect();
        if batch == self.raster_text_batch {
            return;
        }
        self.raster_text_batch.clone_from(&batch);
        if let Some(handle) = self.raster_handle.as_ref() {
            let _ = handle.request_texts(TextBatch {
                document_id,
                pages: batch,
            });
        }
    }

    /// The selection's bands on `page` (glyph boxes merged per line), in
    /// page points.
    pub(super) fn selection_bands_on(&self, page: usize) -> Vec<PtRect> {
        let Some(sel) = self.selection.filter(TextSelection::is_active) else {
            return Vec::new();
        };
        let Some(layer) = self.raster_text.get(&page) else {
            return Vec::new();
        };
        sel.range_on(page, layer.chars.len())
            .map(|(a, b)| layer.bands_for_range(a, b))
            .unwrap_or_default()
    }

    /// `band` on displayed page `page`, on screen.
    fn band_on_screen(&self, page: usize, band: &PtRect) -> Rect {
        let origin = self.viewport.content_point_to_screen(Point::new(
            0.0,
            page.saturating_sub(1) as f64 * self.stack_unit(),
        ));
        let z = self.zoom();
        Rect::new(
            origin.x + band.x * z,
            origin.y + band.y * z,
            origin.x + (band.x + band.w) * z,
            origin.y + (band.y + band.h) * z,
        )
    }

    /// The selection's bands on screen, on the pages the canvas shows.
    pub(super) fn selection_screen_bands(&self) -> Vec<Rect> {
        let Some(sel) = self.selection.filter(TextSelection::is_active) else {
            return Vec::new();
        };
        let visible = self.visible_page_range();
        sel.pages()
            .filter(|page| visible.contains(page))
            .flat_map(|page| {
                self.selection_bands_on(page)
                    .into_iter()
                    .map(move |band| (page, band))
            })
            .map(|(page, band)| self.band_on_screen(page, &band))
            .collect()
    }

    /// The bounds of the selection's visible bands, for anchoring its HUD.
    pub(super) fn selection_screen_bounds(&self) -> Option<Rect> {
        self.selection_screen_bands()
            .into_iter()
            .reduce(|a, b| a.union(b))
    }

    /// On a touchscreen, the grips of a settled selection whose ends are
    /// on screen.
    pub(super) fn selection_grips(&self) -> Option<ui::widget::TextGrips> {
        if self.interaction.modality != ui::interaction::InputModality::Touch {
            return None;
        }
        let sel = self
            .selection
            .filter(|sel| sel.is_active() && !sel.dragging)?;
        let first = self
            .selection_bands_on(sel.start().page)
            .first()
            .map(|band| self.band_on_screen(sel.start().page, band))?;
        let last = self
            .selection_bands_on(sel.end().page)
            .last()
            .map(|band| self.band_on_screen(sel.end().page, band))?;
        Some(ui::widget::TextGrips::new(
            first,
            last,
            f64::from(self.interaction.minimum_target()),
        ))
    }

    /// A press on one of the selection's grips takes that end: the other
    /// end anchors and the grip follows the finger. Whether it landed on one.
    pub(super) fn take_selection_grip(&mut self, p: Point) -> bool {
        let Some(grip) = self.selection_grips().and_then(|grips| grips.grip_at(p)) else {
            return false;
        };
        let Some(sel) = self.selection else {
            return false;
        };
        let (anchor, caret) = match grip {
            ui::widget::TextGrip::Start => (sel.end(), sel.start()),
            ui::widget::TextGrip::End => (sel.start(), sel.end()),
        };
        self.selection = Some(TextSelection {
            anchor,
            caret,
            dragging: true,
        });
        self.sel_gran = Granularity::Char;
        self.dirty = true;
        true
    }

    /// A secondary press (a right click, a held finger) over page text: on
    /// the selection it keeps it, elsewhere it selects the word there. The
    /// selection's one menu (the selection bar) then stands over it, the
    /// same for a pointer and a finger. Whether it was over text.
    pub(super) fn select_for_menu(&mut self, at: Point) -> bool {
        if !self.raster_active() {
            return false;
        }
        let on_selection = self
            .selection_screen_bands()
            .iter()
            .any(|band| band.inflate(2.0, 2.0).contains(at));
        if !on_selection {
            let Some((page, pt)) = self.hit_page(at) else {
                return false;
            };
            let Some(layer) = self.raster_text.get(&page).cloned() else {
                return false;
            };
            let Some(glyph) = layer.char_at(pt.0, pt.1) else {
                return false;
            };
            let (s, e) = layer.word_range(glyph);
            if e <= s {
                return false;
            }
            self.selection = Some(TextSelection {
                anchor: TextPos::new(page, s),
                caret: TextPos::new(page, e),
                dragging: false,
            });
            self.sel_gran = Granularity::Word;
        }
        self.context_menu = None;
        self.dirty = true;
        true
    }

    /// While a selection drag holds the pointer near the canvas's top or
    /// bottom edge (or past it), scroll that way and extend to it. Called
    /// once per frame; whether it scrolled.
    pub(super) fn autoscroll_selection(&mut self, now: ui::time::Instant) -> bool {
        let velocity = self.selection_autoscroll_velocity();
        let dy = self.selection_autoscroll.advance(velocity, now);
        if dy == 0.0 {
            return false;
        }
        let before = self.scroll_y();
        self.viewport.pan_by(0.0, dy);
        self.current_page = self.nearest_page();
        if (self.scroll_y() - before).abs() > f64::EPSILON {
            self.update_selection_at(self.last_mouse);
            self.dirty = true;
            return true;
        }
        false
    }

    /// How fast, in px/s, a selection drag scrolls the canvas: upwards
    /// (negative) in the band at its top edge, downwards at its bottom.
    pub(super) fn selection_autoscroll_velocity(&self) -> f64 {
        if !self.selection.is_some_and(|sel| sel.dragging) {
            return 0.0;
        }
        let canvas = self.canvas_rect;
        ui::edge_autoscroll::velocity(self.last_mouse.y, canvas.y0, canvas.y1)
    }

    /// Highlight the live selection in `swatch`: one highlight per page it
    /// spans, then clear it.
    pub(super) fn highlight_selection(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        swatch: Swatch,
    ) -> bool {
        let Some(sel) = self.selection.filter(TextSelection::is_active) else {
            return false;
        };
        let mut made = Vec::new();
        for page in sel.pages() {
            let bands = self.selection_bands_on(page);
            if bands.is_empty() {
                continue;
            }
            let chars = self.raster_text.get(&page).and_then(|layer| {
                let (a, b) = sel.range_on(page, layer.chars.len())?;
                layer.source_range(a, b)
            });
            made.push(Annotation {
                chars,
                ..Annotation::new(
                    page,
                    AnnoKind::Markup {
                        style: self.markup_style,
                        quads: bands,
                    },
                    swatch,
                    self.author.clone(),
                )
            });
        }
        let Some(last) = made.last().map(|a| a.id.clone()) else {
            return false;
        };
        self.swatch = swatch;
        for annotation in &made {
            self.author_annotation(state, annotation);
        }
        self.selected_anno = Some(last);
        self.clear_selection();
        self.reload(state);
        self.dirty = true;
        true
    }

    /// Attach a sticky note to the start of the live selection and open its
    /// editor (as the ebook reader's note from a selection does).
    pub(super) fn note_from_selection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let Some(sel) = self.selection.filter(TextSelection::is_active) else {
            return false;
        };
        let page = sel.start().page;
        let Some(first) = self.selection_bands_on(page).first().copied() else {
            return false;
        };
        self.clear_selection();
        self.create_note(state, page, (first.x, first.y));
        true
    }
}

/// The caret beside glyph `glyph` of `layer` nearest page x `x`: before it
/// over its leading half, after it over its trailing half.
fn caret_by(layer: &PageText, glyph: usize, x: f64) -> usize {
    let after = layer
        .bands_for_range(glyph, glyph + 1)
        .first()
        .is_some_and(|band| x > band.x + band.w / 2.0);
    glyph + usize::from(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_covers_whole_pages_between_its_ends() {
        let sel = TextSelection {
            anchor: TextPos::new(4, 7),
            caret: TextPos::new(2, 30),
            dragging: false,
        };
        assert_eq!(sel.pages(), 2..=4);
        assert_eq!(sel.range_on(1, 100), None);
        assert_eq!(sel.range_on(2, 100), Some((30, 100)));
        assert_eq!(sel.range_on(3, 50), Some((0, 50)));
        assert_eq!(sel.range_on(4, 100), Some((0, 7)));
        assert_eq!(sel.range_on(3, 0), None, "a page with no text has none");
        let all = TextSelection {
            anchor: TextPos::new(1, 0),
            caret: TextPos::new(3, usize::MAX),
            dragging: false,
        };
        assert_eq!(all.range_on(3, 12), Some((0, 12)));
    }
}
