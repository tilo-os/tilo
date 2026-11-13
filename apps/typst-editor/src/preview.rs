//! The rendered-document preview pane.
//!
//! The compiled pages stacked vertically with cursor-anchored zoom, pan,
//! scrollbars and, past one page, a clickable thumbnail strip. Zoom and pan are
//! the toolkit's shared [`ui::widget::ViewportState`].
//!
//! It also has a text layer: the compiled frames expose the placed glyphs with
//! positions, so a drag over the page selects the covered text and Ctrl+C copies
//! it.

use std::sync::Arc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::MouseScrollDelta;
use winit::keyboard::ModifiersState;

use ui::gesture::{DragSelect, Granularity};
use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::{Selection, extend_span_drag, line_boundaries, word_boundaries};
use ui::theme::Theme;
use ui::tokens::{Spacing, TextRole};
use ui::widget::{
    Banner, BannerContent, BannerKind, EmptyState, EmptyStateContent, SmallContentAlign,
    ViewportState,
};

use crate::pages::TypstPages;
use crate::resource::CATALOG;

mod damage;
mod raster;

#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    page_gap_pt: f64,
    view_pad: f64,
    thumb_w: f64,
    thumb_gap: f64,
}

impl Metrics {
    fn of(spacing: &Spacing) -> Self {
        Self {
            page_gap_pt: f64::from(spacing.s6),
            view_pad: f64::from(spacing.s6),
            thumb_w: f64::from(spacing.s16 + spacing.s3),
            thumb_gap: f64::from(spacing.s3),
        }
    }

    fn strip_w(&self) -> f64 {
        self.thumb_w + 2.0 * self.thumb_gap
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Compiling,
    NoPages,
    Failed,
    Blank,
}

#[derive(Clone, Copy)]
struct PageBox {
    x_off: f64,
    top: f64,
    w_pt: f64,
    h_pt: f64,
}

/// One selectable glyph cell, resolved into **content space** so hit-testing
/// and highlight drawing both go through the viewport transform.
#[derive(Clone, Copy)]
struct Cell<'a> {
    rect: Rect,
    line_key: (usize, i64),
    text: &'a str,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent, mutually-exclusive-in-practice \
              interaction mode (drag kind, fit intent, staleness); a bitflags \
              enum would not read more clearly at call sites like `self.panning`"
)]
pub struct PreviewPane {
    rect: Rect,
    viewport: ViewportState,
    raster: raster::PageRaster,
    boxes: Vec<PageBox>,
    tops: Vec<f64>,
    bottoms: Vec<f64>,
    content_w: f64,
    content_h: f64,
    cell_starts: Vec<usize>,
    selection: Selection,
    drag_select: DragSelect,
    drag_anchor_span: (usize, usize),
    panning: bool,
    dragging_bar: bool,
    fit_width: bool,
    current: usize,
    empty: Empty,
    metrics: Metrics,
    page_pt: Vec<(f64, f64)>,
    banner_h: f64,
    thumb_label_h: f64,
    stale: bool,
    shown: ui::scroll_copy::ShownFrames<damage::PreviewShown>,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            viewport: ViewportState::new()
                .with_padding(Metrics::of(&Spacing::default()).view_pad)
                .with_zoom_bounds(0.1, 8.0)
                .with_alignment(SmallContentAlign::Center, SmallContentAlign::Start),
            raster: raster::PageRaster::default(),
            boxes: Vec::new(),
            tops: Vec::new(),
            bottoms: Vec::new(),
            content_w: 0.0,
            content_h: 0.0,
            cell_starts: Vec::new(),
            selection: Selection::collapsed(0),
            drag_select: DragSelect::new(),
            drag_anchor_span: (0, 0),
            panning: false,
            dragging_bar: false,
            fit_width: true,
            current: 0,
            empty: Empty::Compiling,
            metrics: Metrics::of(&Spacing::default()),
            page_pt: Vec::new(),
            banner_h: 0.0,
            thumb_label_h: 0.0,
            stale: false,
            shown: ui::scroll_copy::ShownFrames::default(),
        }
    }
}

impl PreviewPane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.refit();
    }

    /// Fit the viewport to the pane's current rect, beside the thumbnails and
    /// under the banner.
    fn refit(&mut self) {
        let was_at_top = self.viewport.scroll_y() <= 0.5;
        self.viewport.set_rect(self.main_rect());
        // Re-apply the fit-width intent so a resize keeps the page spanning the
        // pane; a pinned custom zoom is left untouched.
        if self.fit_width {
            self.viewport.set_zoom(self.viewport.fit_width_zoom());
            // `set_zoom` keeps the viewport centre, which is right for a reader who
            // scrolled. At the top of a fresh preview it would manufacture a scroll offset
            // as the fit-width zoom grows, hiding the document title.
            if was_at_top {
                self.viewport.set_scroll(self.viewport.scroll_x(), 0.0);
            }
        }
        self.recompute_current();
    }

    /// Show a fresh compile of the document. Its pages render as they come
    /// into view.
    pub fn set_document(&mut self, document: Arc<TypstPages>) {
        // A reader at the top of the document stays there as the pages change
        // under them; one who scrolled keeps their place.
        let at_top = self.page_pt.is_empty() || self.viewport.scroll_y() <= 0.5;
        self.empty = Empty::NoPages;
        self.page_pt = document.sizes().to_vec();
        self.raster.show(document);

        // Fresh pages supersede any stale render.
        self.stale = false;
        self.restack(at_top);
        self.clear_selection();
    }

    /// Follow `theme`'s spacing scale: the page gap, the viewport padding and
    /// the thumbnail strip. Re-stacks the pages when the scale changed.
    pub fn apply_theme(&mut self, theme: &Theme) {
        let metrics = Metrics::of(&theme.tokens.spacing);
        if metrics == self.metrics {
            return;
        }
        self.metrics = metrics;
        let at_top = self.viewport.scroll_y() <= 0.5;
        let viewport = std::mem::replace(&mut self.viewport, ViewportState::new());
        self.viewport = viewport.with_padding(metrics.view_pad);
        self.restack(at_top);
        self.refit();
    }

    /// Lay the compiled pages out as one stack in page points and rebuild the
    /// text layer over it. `to_top` scrolls back to the top once the fit-width
    /// zoom is re-applied (a fresh render, or a reader who was at the top).
    fn restack(&mut self, to_top: bool) {
        let gap = self.metrics.page_gap_pt;
        let content_w = self
            .page_pt
            .iter()
            .map(|(w, _)| *w)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        let mut boxes = Vec::with_capacity(self.page_pt.len());
        let mut y = 0.0f64;
        for &(w_pt, h_pt) in &self.page_pt {
            let x_off = ((content_w - w_pt) / 2.0).max(0.0);
            boxes.push(PageBox {
                x_off,
                top: y,
                w_pt,
                h_pt,
            });
            y += h_pt + gap;
        }
        let content_h = (y - gap).max(0.0);
        self.tops = boxes.iter().map(|b| b.top).collect();
        self.bottoms = boxes.iter().map(|b| b.top + b.h_pt).collect();
        self.boxes = boxes;
        self.content_w = content_w;
        self.content_h = content_h;
        self.viewport.set_content_size(content_w, content_h);

        self.cell_starts.clear();
        let mut start = 0;
        for page in 0..self.boxes.len() {
            self.cell_starts.push(start);
            start += self
                .raster
                .text(page)
                .map_or(0, crate::page_text::PageText::len);
        }
        self.cell_starts.push(start);

        // Keep the fit-width intent honoured now that the content size changed.
        if self.fit_width {
            self.viewport.set_zoom(self.viewport.fit_width_zoom());
            if to_top {
                self.viewport.set_scroll(self.viewport.scroll_x(), 0.0);
            }
        }
        self.recompute_current();
    }

    fn cell_count(&self) -> usize {
        self.cell_starts.last().copied().unwrap_or(0)
    }

    fn page_cell(&self, page: usize, local: usize) -> Option<Cell<'_>> {
        let text = self.raster.text(page)?;
        let cell = text.cells().get(local)?;
        let pb = self.boxes.get(page)?;
        Some(Cell {
            rect: Rect::new(
                pb.x_off + cell.x0,
                pb.top + cell.top,
                pb.x_off + cell.x1,
                pb.top + cell.bottom,
            ),
            line_key: (page, cell.line),
            text: text.text_of(cell),
        })
    }

    fn cells_in(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = Cell<'_>> + '_ {
        let end = range.end.min(self.cell_count());
        let first_page = self
            .cell_starts
            .partition_point(|start| *start <= range.start)
            .saturating_sub(1);
        (first_page..self.boxes.len())
            .take_while(move |page| {
                self.cell_starts
                    .get(*page)
                    .is_some_and(|start| *start < end)
            })
            .flat_map(move |page| {
                let start = self.cell_starts.get(page).copied().unwrap_or(0);
                let count = self
                    .raster
                    .text(page)
                    .map_or(0, crate::page_text::PageText::len);
                let from = range.start.saturating_sub(start).min(count);
                let to = end.saturating_sub(start).min(count);
                (from..to).filter_map(move |local| self.page_cell(page, local))
            })
    }

    pub fn set_blank(&mut self) {
        self.raster.clear();
        self.page_pt.clear();
        self.stale = false;
        self.empty = Empty::Blank;
        self.clear_selection();
        self.restack(true);
    }

    pub fn is_blank(&self) -> bool {
        self.page_pt.is_empty() && self.empty == Empty::Blank
    }

    /// Whether the pages follow the pane's width (the zoom it opens at and
    /// returns to with Fit width) rather than a zoom the person chose.
    pub fn fits_width(&self) -> bool {
        self.fit_width
    }

    /// Say why there are no pages: the first compile failed rather than is
    /// still running. Ignored while pages are showing.
    pub fn set_failed(&mut self) {
        if self.page_pt.is_empty() {
            self.empty = Empty::Failed;
        }
    }

    /// Mark the on-screen pages stale (the latest compile failed) or fresh. A stale
    /// render keeps showing, with a banner saying it no longer matches the source.
    pub fn set_stale(&mut self, stale: bool) {
        self.stale = stale;
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn page_count(&self) -> usize {
        self.page_pt.len()
    }

    /// Take in the pages the renderer finished. Returns whether any did, so
    /// the window repaints them.
    pub fn take_renders(&mut self) -> bool {
        self.raster.pump()
    }

    /// Whether a page or thumbnail on screen is still waiting for its
    /// sharp bitmap.
    pub fn has_pending_renders(&self) -> bool {
        self.raster.pending_visuals
    }

    /// Whether the renderer runs on the UI thread (no thread could start)
    /// and has work queued, so the window must keep producing frames.
    pub fn wants_frames(&self) -> bool {
        self.raster.wants_frames()
    }

    pub fn current_page(&self) -> usize {
        self.current
    }

    /// Display zoom relative to fit-width, so the toolbar/status read "100 %"
    /// when a page spans the pane — the UX the welcome document advertises.
    pub fn zoom(&self) -> f64 {
        let fw = self.viewport.fit_width_zoom();
        if fw > 0.0 {
            self.viewport.zoom() / fw
        } else {
            1.0
        }
    }

    pub fn zoom_in(&mut self) {
        self.fit_width = false;
        self.viewport.zoom_by(1.25);
        self.recompute_current();
    }

    pub fn zoom_out(&mut self) {
        self.fit_width = false;
        self.viewport.zoom_by(1.0 / 1.25);
        self.recompute_current();
    }

    pub fn fit_width(&mut self) {
        self.fit_width = true;
        self.viewport.set_zoom(self.viewport.fit_width_zoom());
        self.recompute_current();
    }

    fn has_thumbs(&self) -> bool {
        self.page_pt.len() > 1
    }

    /// The main (non-thumbnail) viewport rect: beside the thumbnail strip and
    /// under the stale banner, when they show.
    fn main_rect(&self) -> Rect {
        let x0 = if self.has_thumbs() {
            self.rect.x0 + self.metrics.strip_w()
        } else {
            self.rect.x0
        };
        let y0 = if self.stale && !self.page_pt.is_empty() {
            (self.rect.y0 + self.banner_h).min(self.rect.y1)
        } else {
            self.rect.y0
        };
        Rect::new(x0, y0, self.rect.x1, self.rect.y1)
    }

    fn thumb_strip(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + self.metrics.strip_w(),
            self.rect.y1,
        )
    }

    /// Where each page's thumbnail and its page number sit in the strip, one
    /// layout for both painting and hit-testing. `line` is the height of the
    /// number label.
    fn thumb_slots(&self, line: f64) -> Vec<(Rect, Rect)> {
        let gap = self.metrics.thumb_gap;
        let tw = self.metrics.thumb_w;
        let x0 = self.rect.x0 + gap;
        let mut y = self.rect.y0 + gap;
        let mut slots = Vec::with_capacity(self.page_pt.len());
        for &(pw, ph) in &self.page_pt {
            let th = tw * ph.max(1.0) / pw.max(1.0);
            let image = Rect::new(x0, y, x0 + tw, y + th);
            let label = Rect::new(x0, y + th, x0 + tw, y + th + line);
            slots.push((image, label));
            y += th + line + gap;
        }
        slots
    }

    /// horizontal pan; otherwise vertical pan. A tilt (horizontal) wheel is
    /// folded into the vertical pan when the page can't scroll sideways, so a
    /// tall zoomed page still scrolls no matter which wheel axis fires.
    pub fn on_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        mods: &ModifiersState,
        cursor: Point,
        scale_factor: f64,
    ) {
        let (raw_x, raw_y) = wheel_px(delta, scale_factor);
        let ctrl = mods.control_key() || mods.super_key();
        if ctrl {
            self.fit_width = false;
            let factor = if raw_y >= 0.0 {
                1.0 + raw_y / 240.0
            } else {
                1.0 / (1.0 - raw_y / 240.0)
            };
            self.viewport.zoom_at(factor, cursor);
        } else if mods.shift_key() && self.viewport.overflows_x() {
            self.viewport.pan_by(-raw_y, 0.0);
        } else if self.viewport.overflows_x() {
            self.viewport.pan_by(-raw_x, -raw_y);
        } else {
            // Nothing to scroll horizontally: route any tilt component into the
            // vertical pan so a secondary wheel still moves a tall page.
            self.viewport.pan_by(0.0, -raw_y - raw_x);
        }
        self.recompute_current();
    }

    fn recompute_current(&mut self) {
        if self.boxes.is_empty() {
            self.current = 0;
            return;
        }
        let main = self.viewport.rect();
        let top_content = self
            .viewport
            .screen_to_content(Point::new(main.x0, main.y0))
            .y;
        let mut best = 0usize;
        let mut best_d = f64::MAX;
        for (i, b) in self.boxes.iter().enumerate() {
            let d = (b.top - top_content).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        self.current = best;
    }

    pub fn scroll_to_page(&mut self, i: usize) {
        let Some(b) = self.boxes.get(i).copied() else {
            return;
        };
        let target_y = b.top * self.viewport.zoom();
        self.viewport.set_scroll(self.viewport.scroll_x(), target_y);
        self.current = i.min(self.page_pt.len().saturating_sub(1));
    }

    /// Handle a left-press. Returns `true` when the app should route subsequent
    /// moves back here as a drag (a scrollbar thumb drag or a text selection).
    pub fn on_mouse_down(&mut self, p: Point, shift: bool) -> bool {
        // Thumbnail strip: jump to the clicked page (not a drag).
        if self.has_thumbs() && self.thumb_strip().contains(p) {
            self.thumb_click(p);
            return false;
        }
        // Scrollbars win over the text layer beneath them.
        if self.viewport.press_scrollbar(p) {
            self.dragging_bar = true;
            return true;
        }
        // Past fit-width the page overflows the pane, so a drag pans; at fit-width
        // there is nothing to pan, so a drag selects text.
        if self.viewport.overflows_x() {
            self.viewport.begin_pan(p);
            self.panning = true;
            return true;
        }
        // Begin / extend a text selection. DragSelect owns click counting and
        // the shared text_selection boundaries own word/line semantics.
        if let Some(b) = self.boundary_at(p) {
            let intent = self.drag_select.press(p, shift);
            if intent.extend {
                self.selection.extend_to(b);
                self.drag_anchor_span = (self.selection.anchor, self.selection.anchor);
            } else {
                let (start, end) = match intent.granularity {
                    Granularity::Char => (b, b),
                    Granularity::Word => self.text_span_at(b, false),
                    Granularity::Line => self.text_span_at(b, true),
                };
                self.selection = Selection {
                    anchor: start,
                    cursor: end,
                };
                self.drag_anchor_span = (start, end);
            }
            return true;
        }
        false
    }

    pub fn on_mouse_drag(&mut self, p: Point) {
        if self.dragging_bar {
            self.viewport.drag_scrollbar(p);
            self.recompute_current();
            return;
        }
        if self.panning {
            self.viewport.update_pan(p);
            self.recompute_current();
            return;
        }
        if let Some(granularity) = self.drag_select.drag()
            && let Some(raw) = self.boundary_at(p)
        {
            let (anchor, cursor) = match granularity {
                Granularity::Char => (self.selection.anchor, raw),
                Granularity::Word => {
                    let (start, end) = self.text_span_at(raw, false);
                    extend_span_drag(
                        self.drag_anchor_span.0,
                        self.drag_anchor_span.1,
                        raw,
                        start,
                        end,
                    )
                }
                Granularity::Line => {
                    let (start, end) = self.text_span_at(raw, true);
                    extend_span_drag(
                        self.drag_anchor_span.0,
                        self.drag_anchor_span.1,
                        raw,
                        start,
                        end,
                    )
                }
            };
            self.selection = Selection { anchor, cursor };
        }
    }

    pub fn on_mouse_up(&mut self) {
        if self.dragging_bar {
            self.viewport.release_scrollbar();
            self.dragging_bar = false;
        }
        if self.panning {
            self.viewport.end_pan();
            self.panning = false;
        }
        self.drag_select.release();
    }

    /// Hover feedback for the scrollbar thumbs. Returns whether anything
    /// changed (so the host can schedule a redraw).
    pub fn on_mouse_move(&mut self, p: Point) -> bool {
        self.viewport.update_scrollbar_hover(p)
    }

    /// The nearest selectable cell boundary on the page with text nearest the
    /// point, clamped to the nearest line. `None` when no page has text.
    fn boundary_at(&self, p: Point) -> Option<usize> {
        let cp = self.viewport.screen_to_content(p);
        let page = self.text_page_nearest(cp.y)?;
        let first = *self.cell_starts.get(page)?;
        let count = self.raster.text(page)?.len();
        let cells: Vec<(usize, Cell<'_>)> = (0..count)
            .filter_map(|local| Some((first + local, self.page_cell(page, local)?)))
            .collect();

        // Cells whose vertical band contains the point (the line under it).
        let mut line: Vec<(usize, Cell<'_>)> = cells
            .iter()
            .filter(|(_, c)| cp.y >= c.rect.y0 && cp.y <= c.rect.y1)
            .copied()
            .collect();

        if line.is_empty() {
            // No line under the pointer: snap to the vertically nearest cell,
            // then take every cell sharing that cell's visual line.
            let key = cells
                .iter()
                .min_by(|(_, a), (_, b)| {
                    let da = ((a.rect.y0 + a.rect.y1) * 0.5 - cp.y).abs();
                    let db = ((b.rect.y0 + b.rect.y1) * 0.5 - cp.y).abs();
                    da.total_cmp(&db)
                })?
                .1
                .line_key;
            line = cells
                .iter()
                .filter(|(_, c)| c.line_key == key)
                .copied()
                .collect();
        }

        line.sort_by(|(_, a), (_, b)| a.rect.x0.total_cmp(&b.rect.x0));

        // Left of the line → start; right of it → end.
        let (first, first_cell) = *line.first()?;
        if cp.x <= first_cell.rect.x0 {
            return Some(first);
        }
        let (last, last_cell) = *line.last()?;
        if cp.x >= last_cell.rect.x1 {
            return Some(last + 1);
        }
        for (i, c) in &line {
            let r = c.rect;
            if cp.x < r.x0 {
                return Some(*i);
            }
            if cp.x <= r.x1 {
                let mid = (r.x0 + r.x1) * 0.5;
                return Some(if cp.x < mid { *i } else { i + 1 });
            }
        }
        Some(last + 1)
    }

    /// The page with selectable text nearest the content-space height `y`:
    /// the one under it, else the closest above or below.
    fn text_page_nearest(&self, y: f64) -> Option<usize> {
        let distance = |page: usize| {
            let (top, bottom) = (self.tops.get(page)?, self.bottoms.get(page)?);
            Some(if y < *top {
                top - y
            } else if y > *bottom {
                y - bottom
            } else {
                0.0
            })
        };
        (0..self.boxes.len())
            .filter(|page| self.raster.text(*page).is_some_and(|text| !text.is_empty()))
            .min_by(|a, b| {
                distance(*a)
                    .unwrap_or(f64::MAX)
                    .total_cmp(&distance(*b).unwrap_or(f64::MAX))
            })
    }

    /// The text the shared boundary walkers read: glyph cells as the selectable
    /// units, with visual line breaks as newlines.
    fn logical_text(&self) -> String {
        let mut text = String::new();
        let mut previous = None;
        for cell in self.cells_in(0..self.cell_count()) {
            if let Some(key) = previous
                && key != cell.line_key
            {
                text.push('\n');
            }
            text.push_str(cell.text);
            previous = Some(cell.line_key);
        }
        text
    }

    /// Byte offset before each cell in [`Self::logical_text`], the newline between
    /// visual lines included, so a shared line boundary maps back to the next
    /// line's first cell.
    fn logical_offsets(&self) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(self.cell_count() + 1);
        let mut byte = 0usize;
        let mut previous = None;
        for cell in self.cells_in(0..self.cell_count()) {
            if let Some(key) = previous
                && key != cell.line_key
            {
                byte = byte.saturating_add(1);
            }
            offsets.push(byte);
            byte = byte.saturating_add(cell.text.len());
            previous = Some(cell.line_key);
        }
        offsets.push(byte);
        offsets
    }

    /// The cell boundary nearest a byte boundary from `text_selection`, so word and
    /// line arithmetic stays in the shared module.
    fn cell_boundary_for_byte(&self, byte: usize, offsets: &[usize]) -> usize {
        offsets
            .iter()
            .position(|&offset| offset >= byte)
            .unwrap_or_else(|| self.cell_count())
    }

    fn text_span_at(&self, cell: usize, line: bool) -> (usize, usize) {
        let text = self.logical_text();
        let offsets = self.logical_offsets();
        let byte = offsets
            .get(cell.min(self.cell_count()))
            .copied()
            .unwrap_or(text.len());
        let (start, end) = if line {
            line_boundaries(&text, byte)
        } else {
            word_boundaries(&text, byte)
        };
        (
            self.cell_boundary_for_byte(start, &offsets),
            self.cell_boundary_for_byte(end, &offsets),
        )
    }

    fn thumb_click(&mut self, p: Point) {
        // The label line only places the slots below each image; hit-testing
        // the image and its number alike keeps the whole thumbnail clickable.
        let hit = self
            .thumb_slots(self.thumb_label_h)
            .iter()
            .position(|(image, label)| image.union(*label).contains(p));
        if let Some(i) = hit {
            self.scroll_to_page(i);
        }
    }

    pub fn has_selection(&self) -> bool {
        !self.selection.is_collapsed()
    }

    pub fn clear_selection(&mut self) {
        self.selection = Selection::collapsed(0);
        self.drag_select.release();
    }

    pub fn select_all(&mut self) {
        self.selection = Selection {
            anchor: 0,
            cursor: self.cell_count(),
        };
    }

    /// The selected text, with a newline inserted at every visual line break.
    /// `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let lo = self.selection.min();
        let hi = self.selection.max().min(self.cell_count());
        if lo >= hi {
            return None;
        }
        let mut out = String::new();
        let mut prev_key: Option<(usize, i64)> = None;
        for cell in self.cells_in(lo..hi) {
            if let Some(k) = prev_key
                && k != cell.line_key
            {
                out.push('\n');
            }
            out.push_str(cell.text);
            prev_key = Some(cell.line_key);
        }
        Some(out)
    }

    pub fn demo_set_view(&mut self, display_zoom: f64, scroll_frac: f64) {
        self.fit_width = false;
        let fw = self.viewport.fit_width_zoom();
        self.viewport.set_zoom(fw * display_zoom);
        let max = self.viewport.max_scroll_y();
        self.viewport
            .set_scroll(self.viewport.scroll_x(), max * scroll_frac.clamp(0.0, 1.0));
        self.recompute_current();
    }

    #[allow(
        clippy::similar_names,
        reason = "pan_x_frac/pan_y_frac are a standard x/y coordinate-pair naming convention"
    )]
    pub fn demo_pan_view(&mut self, display_zoom: f64, pan_x_frac: f64, pan_y_frac: f64) {
        self.fit_width = false;
        let fw = self.viewport.fit_width_zoom();
        self.viewport.set_zoom(fw * display_zoom);
        let mx = self.viewport.max_scroll_x();
        let my = self.viewport.max_scroll_y();
        self.viewport.set_scroll(
            mx * pan_x_frac.clamp(0.0, 1.0),
            my * pan_y_frac.clamp(0.0, 1.0),
        );
        self.recompute_current();
    }

    pub fn demo_select_fraction(&mut self, lo: f64, hi: f64) {
        let n = self.cell_count();
        if n == 0 {
            return;
        }
        let anchor = ((n as f64) * lo.clamp(0.0, 1.0)) as usize;
        let cursor = (((n as f64) * hi.clamp(0.0, 1.0)) as usize)
            .max(anchor + 1)
            .min(n);
        self.selection = Selection { anchor, cursor };
    }

    #[cfg(test)]
    pub(crate) fn bitmap_bytes(&self) -> usize {
        self.raster.images.bytes() + self.raster.tiles.bytes()
    }

    #[cfg(test)]
    pub(crate) fn pages_on_screen(&self) -> Vec<usize> {
        let main = self.viewport.rect();
        self.pages_between(main.y0, main.y1)
            .filter(|i| {
                self.page_rect(*i)
                    .is_some_and(|r| r.y1 >= main.y0 && r.y0 <= main.y1)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn blank_pages_on_screen(&self) -> usize {
        self.pages_on_screen()
            .iter()
            .filter(|i| !self.raster.images.keys().any(|(page, _)| page == *i))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn pending_pages(&self) -> std::collections::BTreeSet<usize> {
        self.raster.pending_pages()
    }

    #[cfg(test)]
    pub(crate) fn chapter_on(&self, page: usize) -> Option<usize> {
        let text = self.raster.text(page)?;
        let words: String = text.cells().iter().map(|cell| text.text_of(cell)).collect();
        let after = words.split("Chapter").nth(1)?;
        let digits: String = after
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    }

    #[cfg(test)]
    pub(crate) fn scroll_y(&self) -> f64 {
        self.viewport.scroll_y()
    }

    #[cfg(test)]
    pub(crate) fn pages_rendered(&self) -> u64 {
        self.raster.rendered()
    }

    #[cfg(test)]
    pub(crate) fn delivered_pages(&self) -> Vec<usize> {
        let generation = self.raster.delivered.last().map(|(g, _)| *g);
        self.raster
            .delivered
            .iter()
            .filter(|(g, key)| {
                Some(*g) == generation && key.1 > tilo_page_raster::tiles::THUMB_MAX_PX
            })
            .map(|(_, key)| key.0)
            .collect()
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.page_pt.is_empty() {
            self.draw_empty(scene, transform, tr, theme);
            return;
        }
        // The banner and the number labels are measured here, where the text
        // renderer is; a change re-fits the viewport beside them.
        let banner_h = if self.stale {
            f64::from(theme.tokens.metrics.panel_row_height)
        } else {
            0.0
        };
        let label_h = f64::from(
            tr.measure_style("0", None, &theme.text(TextRole::Caption))
                .1,
        );
        if (banner_h - self.banner_h).abs() > 0.01 || (label_h - self.thumb_label_h).abs() > 0.01 {
            self.banner_h = banner_h;
            self.thumb_label_h = label_h;
            self.refit();
        }

        let slots = if self.has_thumbs() {
            self.thumb_slots(self.thumb_label_h)
        } else {
            Vec::new()
        };
        let frame = self.plan_frame(&slots);
        self.draw_main(scene, transform, tr, theme, &frame);
        if self.has_thumbs() {
            self.draw_thumbs(scene, transform, tr, theme, &slots, &frame);
        }
        if self.stale {
            self.draw_stale_banner(scene, transform, tr, theme);
        }
        self.note_shown(transform, theme, &frame, &slots);
    }

    fn draw_stale_banner(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let gap = f64::from(theme.tokens.spacing.s3);
        let area = Rect::new(
            self.main_rect().x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.banner_h,
        );
        let banner = area.inset((-gap, -gap / 2.0));
        let content = BannerContent::new(BannerKind::Warning, "Showing the last successful render");
        Banner::paint(
            &mut PaintCtx::new(scene, transform, tr, theme),
            banner,
            &content,
            Point::new(-1.0, -1.0),
        );
    }

    fn draw_main(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        frame: &raster::FramePages,
    ) {
        let c = &theme.tokens.colors;
        let main = self.viewport.rect();
        let page_count = self.page_pt.len();
        let caption = theme.text(TextRole::Caption).numeric();
        let caption_gap = f64::from(theme.tokens.spacing.s2);
        let page_radius = f64::from(theme.tokens.radii.detail);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &main);

        for i in self.pages_between(main.y0, main.y1) {
            let Some(page_rect) = self.page_rect(i) else {
                continue;
            };
            if page_rect.y1 < main.y0 || page_rect.y0 > main.y1 {
                continue;
            }
            // The page lifts off the pane: the elevation token's shadow, then
            // the paper, then its derived edge.
            ui::surface::shadow(
                scene,
                transform,
                page_rect,
                page_radius,
                &theme.tokens.elevation.floating,
            );
            tilo_page_raster::paint_page(
                scene,
                transform,
                page_rect,
                frame.pages.get(&i),
                frame.tiles.get(&i),
            );
            ui::paint::stroke_rect(
                scene,
                transform,
                page_rect,
                ui::surface::edge_on(c.surfaces.content),
                1.0,
            );

            // Page caption under the page.
            let label = CATALOG.t_args(
                "typst-editor-page-of",
                &[
                    ("page", ArgValue::Text((i + 1).to_string())),
                    ("pages", ArgValue::Text(page_count.to_string())),
                ],
            );
            tr.draw_style(
                scene,
                &label,
                c.text_dim,
                transform * Affine::translate((page_rect.x0, page_rect.y1 + caption_gap)),
                None,
                &caption,
            );
        }

        self.draw_selection(scene, transform, theme, main);

        scene.pop_layer();

        // Scrollbars paint on top, outside the page clip.
        self.viewport.draw_scrollbars(scene, transform, theme);
    }

    fn draw_selection(&self, scene: &mut Scene, transform: Affine, theme: &Theme, main: Rect) {
        if !self.has_selection() {
            return;
        }
        // Only the selected cells on the pages in view can paint.
        let shown = self.pages_between(main.y0, main.y1);
        let on_screen = |page: usize| self.cell_starts.get(page).copied().unwrap_or(0);
        let lo = self.selection.min().max(on_screen(shown.start));
        let hi = self
            .selection
            .max()
            .min(self.cell_count())
            .min(on_screen(shown.end));
        let fill = theme.tokens.colors.intent.selection_tint;

        let mut run: Option<((usize, i64), Rect)> = None;
        let flush = |scene: &mut Scene, band: Rect, vp: &ViewportState| {
            let tl = vp.content_point_to_screen(Point::new(band.x0, band.y0));
            let br = vp.content_point_to_screen(Point::new(band.x1, band.y1));
            let r = Rect::new(tl.x, tl.y, br.x, br.y);
            if r.y1 >= main.y0 && r.y0 <= main.y1 {
                scene.fill(Fill::NonZero, transform, fill, None, &r);
            }
        };
        for cell in self.cells_in(lo..hi) {
            match &mut run {
                Some((key, band)) if *key == cell.line_key => {
                    band.x1 = band.x1.max(cell.rect.x1);
                    band.y0 = band.y0.min(cell.rect.y0);
                    band.y1 = band.y1.max(cell.rect.y1);
                }
                _ => {
                    if let Some((_, band)) = run.take() {
                        flush(scene, band, &self.viewport);
                    }
                    run = Some((cell.line_key, cell.rect));
                }
            }
        }
        if let Some((_, band)) = run.take() {
            flush(scene, band, &self.viewport);
        }
    }

    fn draw_thumbs(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        slots: &[(Rect, Rect)],
        frame: &raster::FramePages,
    ) {
        let c = &theme.tokens.colors;
        let strip = self.thumb_strip();
        // The strip stands on the pane's ground; one hairline separates it
        // from the pages.
        ui::paint::fill_rect(
            scene,
            transform,
            Rect::new(strip.x1 - 1.0, strip.y0, strip.x1, strip.y1),
            ui::surface::edge_on(c.surfaces.content),
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &strip);

        let number = theme.text(TextRole::Caption).numeric();
        let gap = self.metrics.thumb_gap;
        let current = self.current;
        let item_radius = f64::from(theme.tokens.radii.item);
        for (i, (image, label_rect)) in slots.iter().enumerate() {
            if image.y0 > strip.y1 {
                break;
            }
            // The page the preview is on is the lifted one.
            if i == current {
                let lift = c.lift_on(c.surfaces.content, 1.0);
                // A positive inset grows the rect: the plate surrounds the
                // thumbnail and its number with half a gap to spare.
                let plate = image.union(*label_rect).inset(gap / 2.0);
                ui::paint::fill_round(scene, transform, plate, item_radius, lift);
            }
            tilo_page_raster::paint_page(scene, transform, *image, frame.thumbs.get(&i), None);
            ui::paint::stroke_rect(
                scene,
                transform,
                *image,
                ui::surface::edge_on(c.surfaces.content),
                1.0,
            );
            let text = (i + 1).to_string();
            let (w, _) = tr.measure_style(&text, None, &number);
            tr.draw_style(
                scene,
                &text,
                if i == current {
                    c.text_primary
                } else {
                    c.text_dim
                },
                transform
                    * Affine::translate((
                        label_rect.center().x - f64::from(w) / 2.0,
                        label_rect.y0,
                    )),
                None,
                &number,
            );
        }

        scene.pop_layer();
    }

    fn draw_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let content = match self.empty {
            Empty::Compiling => EmptyStateContent::new("Compiling the document"),
            Empty::NoPages => EmptyStateContent {
                icon: Some(Icon::FileText),
                message: Some("The document has no pages."),
                ..EmptyStateContent::new("Nothing to preview")
            },
            Empty::Failed => EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: Some("Fix the errors listed under Problems."),
                error: true,
                ..EmptyStateContent::new("The document did not compile")
            },
            Empty::Blank => EmptyStateContent {
                icon: Some(Icon::FileText),
                message: Some("Write Typst markup in the source and its pages appear here."),
                ..EmptyStateContent::new("The document is empty")
            },
        };
        EmptyState::paint(
            &mut PaintCtx::new(scene, transform, tr, theme),
            self.rect,
            &content,
            Point::new(-1.0, -1.0),
        );
    }
}

fn wheel_px(delta: &MouseScrollDelta, scale_factor: f64) -> (f64, f64) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 56.0, f64::from(*y) * 56.0),
        MouseScrollDelta::PixelDelta(pos) => (pos.x / scale_factor, pos.y / scale_factor),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::page_text::PageText;
    use winit::keyboard::ModifiersState;

    fn pages(sizes: &[(f64, f64)]) -> Arc<TypstPages> {
        Arc::new(TypstPages::blank(sizes))
    }

    fn page_text(lines: &[(f64, &str, f64)]) -> PageText {
        let mut text = PageText::default();
        for (y, chars, x0) in lines {
            let cells: Vec<(f64, f64, String)> = chars
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    let x = x0 + 8.0 * i as f64;
                    (x, x + 8.0, ch.to_string())
                })
                .collect();
            text.push_run(
                *y,
                y + 12.0,
                cells.iter().map(|(x0, x1, ch)| (*x0, *x1, ch.as_str())),
            );
        }
        text
    }

    fn text_page(size: (f64, f64), text: PageText) -> Arc<TypstPages> {
        Arc::new(TypstPages::blank(&[size]).with_text(vec![text]))
    }

    fn pane() -> PreviewPane {
        let mut p = PreviewPane::new();
        p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        p.set_document(text_page(
            (200.0, 800.0),
            page_text(&[(40.0, "Hello world", 20.0)]),
        ));
        p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        p
    }

    #[test]
    fn preview_zooms_fits_and_switches_pages() {
        {
            let mut p = pane();
            let z0 = p.viewport.zoom();
            let center = p.viewport.rect().center();
            p.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, 1.0),
                &ModifiersState::CONTROL,
                center,
                1.0,
            );
            assert!(p.viewport.zoom() > z0, "ctrl+wheel-up zooms in");
            assert!(!p.fit_width, "an explicit zoom drops the fit-width intent");
        }
        {
            let p = pane();
            assert_eq!(
                p.viewport.scroll_y(),
                0.0,
                "a first fit-width layout must not centre-scroll past the title"
            );
        }
        {
            let mut p = PreviewPane::new();
            p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            p.set_document(pages(&[(200.0, 260.0), (200.0, 260.0), (200.0, 260.0)]));
            p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            assert!(p.has_thumbs());
            p.scroll_to_page(2);
            assert_eq!(p.current_page(), 2);
            assert!(
                p.viewport.scroll_y() > 0.0,
                "jumping to a later page scrolls down"
            );
        }
    }

    #[test]
    fn preview_text_selection_copies_words_drags_and_all() {
        {
            let mut p = pane();
            assert_eq!(p.text_span_at(7, false), (6, 11));

            // The second word is selected by the shared double-click classifier;
            // a subsequent drag extends in whole shared word units.
            let point = p
                .viewport
                .content_point_to_screen(Point::new(20.0 + 7.5 * 8.0, 46.0));
            assert!(p.on_mouse_down(point, false));
            p.on_mouse_up();
            assert!(p.on_mouse_down(point, false));
            assert!(p.has_selection());
            assert_eq!(p.selected_text().as_deref(), Some("world"));
            p.on_mouse_up();
        }
        {
            let mut p = pane();
            // Press at the first glyph, drag to just before the space (5 chars in).
            let start = p.viewport.content_point_to_screen(Point::new(21.0, 46.0));
            let end = p
                .viewport
                .content_point_to_screen(Point::new(20.0 + 8.0 * 5.0 + 1.0, 46.0));
            assert!(
                p.on_mouse_down(start, false),
                "a text press begins a selection drag"
            );
            p.on_mouse_drag(end);
            assert!(p.has_selection(), "the drag produced a selection");
            assert_eq!(p.selected_text().as_deref(), Some("Hello"));
        }
        {
            let mut p = PreviewPane::new();
            p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            p.set_document(text_page(
                (200.0, 400.0),
                page_text(&[(40.0, "ab", 20.0), (80.0, "cd", 20.0)]),
            ));
            p.select_all();
            assert_eq!(
                p.selected_text().as_deref(),
                Some("ab\ncd"),
                "line break between runs"
            );
        }
    }

    #[test]
    fn preview_wheel_and_drag_scroll_or_pan() {
        {
            let mut p = pane();
            // Even at fit-width a 200×800pt page overflows the 300px pane vertically.
            assert!(p.viewport.overflows_y(), "tall page overflows vertically");
            p.demo_set_view(1.6, 0.3);
            let y0 = p.viewport.scroll_y();
            p.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, -1.0),
                &ModifiersState::empty(),
                p.viewport.rect().center(),
                1.0,
            );
            assert!(
                p.viewport.scroll_y() > y0,
                "plain wheel scrolls the zoomed page down"
            );
        }
        {
            let mut p = pane();
            // Fit-width: nothing to scroll sideways, but the page is tall.
            assert!(!p.viewport.overflows_x());
            assert!(p.viewport.overflows_y());
            p.demo_set_view(1.0, 0.3); // move off the top so a scroll can register
            let y0 = p.viewport.scroll_y();
            // A tilt (horizontal) wheel is folded into vertical panning.
            p.on_wheel(
                &MouseScrollDelta::LineDelta(1.0, 0.0),
                &ModifiersState::empty(),
                p.viewport.rect().center(),
                1.0,
            );
            assert_ne!(
                p.viewport.scroll_y(),
                y0,
                "tilt wheel moves a tall page vertically"
            );
        }
        {
            let mut p = pane();
            // Zoom in past fit-width so the page overflows horizontally.
            p.demo_set_view(1.6, 0.3);
            assert!(
                p.viewport.overflows_x(),
                "zoomed past fit-width the page overflows horizontally"
            );
            let sx0 = p.viewport.scroll_x();
            let start = p.viewport.rect().center();
            // A press while zoomed begins a pan drag (not a text selection).
            assert!(p.on_mouse_down(start, false), "a press while zoomed pans");
            p.on_mouse_drag(Point::new(start.x - 60.0, start.y - 20.0));
            assert!(!p.has_selection(), "a pan drag never selects text");
            assert!(
                p.viewport.scroll_x() > sx0,
                "dragging left reveals content to the right (scroll_x grew: {} -> {})",
                sx0,
                p.viewport.scroll_x()
            );
            p.on_mouse_up();
            assert!(!p.viewport.is_panning(), "releasing ends the pan");
        }
        {
            let mut p = pane();
            p.demo_set_view(1.6, 0.3);
            assert!(
                p.viewport.overflows_x(),
                "page overflows sideways when zoomed"
            );
            let x0 = p.viewport.scroll_x();
            // A tilt (horizontal) wheel pans the page sideways once it overflows.
            p.on_wheel(
                &MouseScrollDelta::LineDelta(-1.0, 0.0),
                &ModifiersState::empty(),
                p.viewport.rect().center(),
                1.0,
            );
            assert_ne!(
                p.viewport.scroll_x(),
                x0,
                "tilt wheel pans a wide zoomed page sideways"
            );
        }
    }
}
