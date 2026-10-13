//! The rendered-document preview pane.
//!
//! Shows the compiled pages stacked vertically with cursor-anchored zoom, pan,
//! scrollbars, and — when the document has more than one page — a clickable
//! thumbnail strip. Zoom/pan is delegated to the toolkit's shared
//! [`ui::widget::ViewportState`] (the same kernel the PDF, ebook, and maps
//! viewers use), so the interaction is consistent across the OS: Ctrl+wheel
//! zooms toward the cursor, a plain wheel scrolls, and the content stays
//! reachable when it overflows the pane.
//!
//! The pane also carries a **text layer**: `typst`'s compiled frames expose the
//! placed glyphs with positions, so a drag over the rendered page selects the
//! covered text (mapped back through the same content→screen transform) and
//! Ctrl+C copies it. Selection is glyph-precise horizontally and line-precise
//! vertically.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::MouseScrollDelta;
use winit::keyboard::ModifiersState;

use ui::gesture::{DragSelect, Granularity};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::{Selection, extend_span_drag, line_boundaries, word_boundaries};
use ui::theme::Theme;
use ui::tokens::{Spacing, TextRole};
use ui::widget::{
    Banner, BannerContent, BannerKind, EmptyState, EmptyStateContent, Image, ImageFit,
    SmallContentAlign, ViewportState,
};

use crate::compile::{PageImage, TextRun};

/// The preview's geometry, every length a step of the theme's spacing scale.
#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    /// Vertical gap between stacked pages, in **content** (page-point) units,
    /// so it scales with zoom exactly like the pages.
    page_gap_pt: f64,
    /// Screen padding the viewport reserves around the page stack.
    view_pad: f64,
    /// Width of a page thumbnail.
    thumb_w: f64,
    /// Space around and between the thumbnails.
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

    /// Width of the thumbnail strip.
    fn strip_w(&self) -> f64 {
        self.thumb_w + 2.0 * self.thumb_gap
    }
}

/// Why the pane shows no pages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Compiling,
    NoPages,
    Failed,
}

/// A page's placement within the content-space page stack (all in page points).
#[derive(Clone, Copy)]
struct PageBox {
    /// Horizontal offset of the page's left edge inside the content box
    /// (centres narrower pages). Page points.
    x_off: f64,
    /// Top of the page inside the content box. Page points.
    top: f64,
    w_pt: f64,
    h_pt: f64,
}

/// One selectable glyph cell, resolved into **content space** so hit-testing
/// and highlight drawing both go through the viewport transform.
#[derive(Clone)]
struct Cell {
    rect: Rect,
    /// A stable key for the visual line the cell sits on (page + quantised
    /// baseline), used to insert newlines when copying across lines.
    line_key: (usize, i64),
    text: String,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent, mutually-exclusive-in-practice \
              interaction mode (drag kind, fit intent, staleness); a bitflags \
              enum would not read more clearly at call sites like `self.panning`"
)]
pub struct PreviewPane {
    rect: Rect,
    /// Shared zoom/pan kernel; its rect is the main viewport (pane minus the
    /// thumbnail strip).
    viewport: ViewportState,
    images: Vec<Image>,
    /// Intrinsic page sizes in px (width, height) — for thumbnail aspect.
    sizes_px: Vec<(u32, u32)>,
    /// Page placement in the content-space stack (page points).
    boxes: Vec<PageBox>,
    /// Content-space dimensions of the whole page stack (page points).
    content_w: f64,
    content_h: f64,
    /// Selectable glyph cells in reading order (content space).
    cells: Vec<Cell>,
    /// Active selection as shared anchor/cursor state over cell-boundary
    /// indices. The rendered page is read-only, so this is the same shared
    /// selection model used by the other non-editable readers.
    selection: Selection,
    /// Shared click-count classifier for char/word/line selection gestures.
    drag_select: DragSelect,
    /// The whole unit selected by the press that started a word/line drag.
    drag_anchor_span: (usize, usize),
    /// A drag-to-pan (hand-tool) gesture is in progress. Only begins when the
    /// page is zoomed past fit-width and therefore overflows the pane.
    panning: bool,
    /// A scrollbar thumb drag is in progress.
    dragging_bar: bool,
    /// Whether "fit width" is the active zoom intent (vs. a pinned custom zoom).
    fit_width: bool,
    /// Page index considered "current" (top-most visible) — drives the
    /// thumbnail highlight and the status bar.
    current: usize,
    /// Why there are no pages to show.
    empty: Empty,
    metrics: Metrics,
    /// Intrinsic page sizes in page points, for re-stacking when the spacing
    /// scale changes.
    page_pt: Vec<(f64, f64)>,
    page_text: Vec<Vec<TextRun>>,
    /// Height of the stale-render banner, reserved above the pages while it
    /// shows; measured at paint.
    banner_h: f64,
    /// Height of one thumbnail number label; measured at paint.
    thumb_label_h: f64,
    /// The pages on screen are from an earlier successful compile — the latest
    /// compile failed, so we keep the last good render and flag it as stale
    /// rather than blanking the pane and losing the reader's place.
    stale: bool,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            viewport: ViewportState::new()
                .with_padding(Metrics::of(&Spacing::default()).view_pad)
                .with_zoom_bounds(0.1, 8.0)
                .with_alignment(SmallContentAlign::Center, SmallContentAlign::Start),
            images: Vec::new(),
            sizes_px: Vec::new(),
            boxes: Vec::new(),
            content_w: 0.0,
            content_h: 0.0,
            cells: Vec::new(),
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
            page_text: Vec::new(),
            banner_h: 0.0,
            thumb_label_h: 0.0,
            stale: false,
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
            // `set_zoom` preserves the viewport centre, which is right for a
            // reader who deliberately scrolled.  On a fresh/top-of-document
            // preview it would instead manufacture a scroll offset while the
            // initial fit-width zoom grows, hiding the document title.
            if was_at_top {
                self.viewport.set_scroll(self.viewport.scroll_x(), 0.0);
            }
        }
        self.recompute_current();
    }

    /// Install freshly compiled pages together with their extracted text layer.
    /// `page_text` is parallel to `pages` (empty when the compile failed).
    pub fn set_pages_with_text(&mut self, pages: &[PageImage], page_text: &[Vec<TextRun>]) {
        // A reader at the top of the document stays there as the pages change
        // under them; one who scrolled keeps their place.
        let at_top = self.images.is_empty() || self.viewport.scroll_y() <= 0.5;
        self.images = pages
            .iter()
            .map(|p| Image::from_rgba(p.rgba.clone(), p.width, p.height).with_fit(ImageFit::Fill))
            .collect();
        self.sizes_px = pages.iter().map(|p| (p.width, p.height)).collect();
        self.empty = Empty::NoPages;
        self.page_pt = pages.iter().map(|p| (p.width_pt, p.height_pt)).collect();
        self.page_text = page_text.to_vec();

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
        self.boxes = boxes;
        self.content_w = content_w;
        self.content_h = content_h;
        self.viewport.set_content_size(content_w, content_h);

        // Flatten the per-page runs into content-space cells in reading order.
        let page_text = std::mem::take(&mut self.page_text);
        self.rebuild_cells(&page_text);
        self.page_text = page_text;

        // Keep the fit-width intent honoured now that the content size changed.
        if self.fit_width {
            self.viewport.set_zoom(self.viewport.fit_width_zoom());
            if to_top {
                self.viewport.set_scroll(self.viewport.scroll_x(), 0.0);
            }
        }
        self.recompute_current();
    }

    fn rebuild_cells(&mut self, page_text: &[Vec<TextRun>]) {
        let mut cells = Vec::new();
        for (pi, runs) in page_text.iter().enumerate() {
            let Some(pb) = self.boxes.get(pi).copied() else {
                continue;
            };
            for run in runs {
                let cy0 = pb.top + run.top;
                let cy1 = pb.top + run.bottom;
                let line_key = (pi, ((pb.top + run.top) * 2.0).round() as i64);
                for c in &run.cells {
                    cells.push(Cell {
                        rect: Rect::new(pb.x_off + c.x0, cy0, pb.x_off + c.x1, cy1),
                        line_key,
                        text: c.text.clone(),
                    });
                }
            }
        }
        self.cells = cells;
    }

    /// Say why there are no pages: the first compile failed rather than is
    /// still running. Ignored while pages are showing.
    pub fn set_failed(&mut self) {
        if self.images.is_empty() {
            self.empty = Empty::Failed;
        }
    }

    /// Mark the on-screen pages as stale (the latest compile failed) or fresh.
    /// A stale render keeps showing but wears a banner so the user knows the
    /// preview no longer matches the source.
    pub fn set_stale(&mut self, stale: bool) {
        self.stale = stale;
    }

    /// Whether the on-screen pages are from an earlier, superseded compile.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn page_count(&self) -> usize {
        self.images.len()
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
        self.images.len() > 1
    }

    /// The main (non-thumbnail) viewport rect: beside the thumbnail strip and
    /// under the stale banner, when they show.
    fn main_rect(&self) -> Rect {
        let x0 = if self.has_thumbs() {
            self.rect.x0 + self.metrics.strip_w()
        } else {
            self.rect.x0
        };
        let y0 = if self.stale && !self.images.is_empty() {
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
        let mut slots = Vec::with_capacity(self.sizes_px.len());
        for &(pw, ph) in &self.sizes_px {
            let th = tw * f64::from(ph.max(1)) / f64::from(pw.max(1));
            let image = Rect::new(x0, y, x0 + tw, y + th);
            let label = Rect::new(x0, y + th, x0 + tw, y + th + line);
            slots.push((image, label));
            y += th + line + gap;
        }
        slots
    }

    // ----- scrolling / zoom input ----------------------------------------

    /// Reduce a mouse-wheel event: Ctrl/Cmd → cursor-anchored zoom; Shift →
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

    /// Scroll so page `i` sits near the top of the viewport.
    pub fn scroll_to_page(&mut self, i: usize) {
        let Some(b) = self.boxes.get(i).copied() else {
            return;
        };
        let target_y = b.top * self.viewport.zoom();
        self.viewport.set_scroll(self.viewport.scroll_x(), target_y);
        self.current = i.min(self.images.len().saturating_sub(1));
    }

    // ----- pointer -------------------------------------------------------

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
        // Zoomed past fit-width the page overflows the pane, so a drag is a
        // hand-tool pan (the same gesture the PDF/ebook/maps viewers use). At
        // fit-width there's nothing to pan, so a drag selects text instead —
        // keeping both gestures reachable without a modifier key.
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

    /// Map a screen point to the nearest selectable cell boundary. Clamps to
    /// the nearest line when the pointer is in the margin. `None` when the
    /// page carries no selectable text at all, or — a defensive fallback that
    /// should never trigger since every index below comes from `self.cells`
    /// itself — an index built from it fails to resolve.
    fn boundary_at(&self, p: Point) -> Option<usize> {
        if self.cells.is_empty() {
            return None;
        }
        let cp = self.viewport.screen_to_content(p);

        // Cells whose vertical band contains the point (the line under it).
        let mut line: Vec<usize> = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| cp.y >= c.rect.y0 && cp.y <= c.rect.y1)
            .map(|(i, _)| i)
            .collect();

        if line.is_empty() {
            // No line under the pointer: snap to the vertically nearest cell,
            // then take every cell sharing that cell's visual line.
            let mut nearest = 0usize;
            let mut best = f64::MAX;
            for (i, c) in self.cells.iter().enumerate() {
                let cy = (c.rect.y0 + c.rect.y1) * 0.5;
                let d = (cy - cp.y).abs();
                if d < best {
                    best = d;
                    nearest = i;
                }
            }
            let key = self.cells.get(nearest)?.line_key;
            line = self
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.line_key == key)
                .map(|(i, _)| i)
                .collect();
        }

        line.sort_by(|&a, &b| {
            let xa = self.cells.get(a).map(|c| c.rect.x0);
            let xb = self.cells.get(b).map(|c| c.rect.x0);
            xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Left of the line → start; right of it → end.
        let &first = line.first()?;
        let first_cell = self.cells.get(first)?;
        if cp.x <= first_cell.rect.x0 {
            return Some(first);
        }
        let &last = line.last()?;
        let last_cell = self.cells.get(last)?;
        if cp.x >= last_cell.rect.x1 {
            return Some(last + 1);
        }
        for &i in &line {
            let Some(r) = self.cells.get(i).map(|c| c.rect) else {
                continue;
            };
            if cp.x < r.x0 {
                return Some(i);
            }
            if cp.x <= r.x1 {
                let mid = (r.x0 + r.x1) * 0.5;
                return Some(if cp.x < mid { i } else { i + 1 });
            }
        }
        Some(last + 1)
    }

    /// Reconstruct the logical read-only text used by the shared boundary
    /// walkers. Glyph cells are kept as the selectable units; visual line
    /// breaks become newlines only for word/line boundary calculation and
    /// clipboard output.
    fn logical_text(&self) -> String {
        let mut text = String::new();
        let mut previous = None;
        for cell in &self.cells {
            if let Some(key) = previous
                && key != cell.line_key
            {
                text.push('\n');
            }
            text.push_str(&cell.text);
            previous = Some(cell.line_key);
        }
        text
    }

    /// Byte offset before each cell in [`Self::logical_text`]. The extra
    /// newline between visual lines is deliberately included so the shared
    /// line-boundary result maps back to the first cell of the next line.
    fn logical_offsets(&self) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(self.cells.len() + 1);
        let mut byte = 0usize;
        let mut previous = None;
        for cell in &self.cells {
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

    /// Convert a byte boundary returned by `text_selection` into the nearest
    /// cell boundary. This keeps all UTF-8/word/line arithmetic in the shared
    /// module while the preview continues to select shaped glyph cells.
    fn cell_boundary_for_byte(&self, byte: usize, offsets: &[usize]) -> usize {
        offsets
            .iter()
            .position(|&offset| offset >= byte)
            .unwrap_or(self.cells.len())
    }

    fn text_span_at(&self, cell: usize, line: bool) -> (usize, usize) {
        let text = self.logical_text();
        let offsets = self.logical_offsets();
        let byte = offsets
            .get(cell.min(self.cells.len()))
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

    // ----- selection API used by the app --------------------------------

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
            cursor: self.cells.len(),
        };
    }

    /// The selected text, with a newline inserted at every visual line break.
    /// `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let lo = self.selection.min();
        let hi = self.selection.max().min(self.cells.len());
        if lo >= hi {
            return None;
        }
        let mut out = String::new();
        let mut prev_key: Option<(usize, i64)> = None;
        let selected = self.cells.get(lo..hi)?;
        for cell in selected {
            if let Some(k) = prev_key
                && k != cell.line_key
            {
                out.push('\n');
            }
            out.push_str(&cell.text);
            prev_key = Some(cell.line_key);
        }
        Some(out)
    }

    // ----- screenshot-only arrangement ----------------------------------

    /// Zoom the preview to `display_zoom`× fit-width and scroll to `scroll_frac`
    /// of the way down. Used only by the offscreen `--shots` renderer.
    pub fn demo_set_view(&mut self, display_zoom: f64, scroll_frac: f64) {
        self.fit_width = false;
        let fw = self.viewport.fit_width_zoom();
        self.viewport.set_zoom(fw * display_zoom);
        let max = self.viewport.max_scroll_y();
        self.viewport
            .set_scroll(self.viewport.scroll_x(), max * scroll_frac.clamp(0.0, 1.0));
        self.recompute_current();
    }

    /// Zoom past fit-width and pan to `(pan_x_frac, pan_y_frac)` of the reachable
    /// scroll range, so an offscreen capture can prove the page pans on both
    /// axes once it overflows the pane. Used only by the `--shots` renderer.
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

    /// Select the cells between two fractions of the reading-order text. Used
    /// only by the offscreen `--shots` renderer.
    pub fn demo_select_fraction(&mut self, lo: f64, hi: f64) {
        let n = self.cells.len();
        if n == 0 {
            return;
        }
        let anchor = ((n as f64) * lo.clamp(0.0, 1.0)) as usize;
        let cursor = (((n as f64) * hi.clamp(0.0, 1.0)) as usize)
            .max(anchor + 1)
            .min(n);
        self.selection = Selection { anchor, cursor };
    }

    // ----- drawing -------------------------------------------------------

    /// Paint the preview on the ground of `theme`, which is the region the
    /// pane sits in: the pane fills nothing of its own.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.images.is_empty() {
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

        self.draw_main(scene, transform, tr, theme);
        if self.has_thumbs() {
            self.draw_thumbs(scene, transform, tr, theme);
        }
        if self.stale {
            self.draw_stale_banner(scene, transform, tr, theme);
        }
    }

    /// The banner across the top of the preview telling the user the pages
    /// below are from the last good compile, not the current source.
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
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let main = self.viewport.rect();
        let page_count = self.images.len();
        let caption = theme.text(TextRole::Caption).numeric();
        let caption_gap = f64::from(theme.tokens.spacing.s2);
        let page_radius = f64::from(theme.tokens.radii.detail);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &main);

        for (i, (b, image)) in self.boxes.iter().zip(self.images.iter_mut()).enumerate() {
            let tl = self
                .viewport
                .content_point_to_screen(Point::new(b.x_off, b.top));
            let br = self
                .viewport
                .content_point_to_screen(Point::new(b.x_off + b.w_pt, b.top + b.h_pt));
            let page_rect = Rect::new(tl.x, tl.y, br.x, br.y);

            // Cull pages outside the viewport.
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
            image.paint_at(scene, transform, page_rect);
            ui::paint::stroke_rect(
                scene,
                transform,
                page_rect,
                ui::surface::edge_on(c.surfaces.content),
                1.0,
            );

            // Page caption under the page.
            let label = format!("Page {} of {page_count}", i + 1);
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

    /// Paint the selection as translucent bands, merging adjacent selected
    /// cells on the same line into one rect so it reads as a continuous
    /// highlight.
    fn draw_selection(&self, scene: &mut Scene, transform: Affine, theme: &Theme, main: Rect) {
        if !self.has_selection() {
            return;
        }
        let lo = self.selection.min();
        let hi = self.selection.max().min(self.cells.len());
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
        let Some(selected) = self.cells.get(lo..hi) else {
            return;
        };
        for cell in selected {
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
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
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
        let slots = self.thumb_slots(self.thumb_label_h);
        for (i, (image, label_rect)) in slots.iter().enumerate() {
            // The page the preview is on is the lifted one.
            if i == current {
                let lift = c.lift_on(c.surfaces.content, 1.0);
                // A positive inset grows the rect: the plate surrounds the
                // thumbnail and its number with half a gap to spare.
                let plate = image.union(*label_rect).inset(gap / 2.0);
                ui::paint::fill_round(scene, transform, plate, item_radius, lift);
            }
            if let Some(thumb) = self.images.get_mut(i) {
                thumb.paint_at(scene, transform, *image);
            }
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

    /// The pane while it has no pages: the compile is running, produced
    /// nothing, or failed before anything rendered.
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
        };
        EmptyState::paint(
            &mut PaintCtx::new(scene, transform, tr, theme),
            self.rect,
            &content,
            Point::new(-1.0, -1.0),
        );
    }
}

/// Normalise a wheel delta into logical pixels per axis (matching the shared
/// viewport kernel's own scaling so wheel feel is identical across apps).
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
    use crate::compile::{PageImage, TextCell, TextRun};
    use std::sync::Arc;
    use winit::keyboard::ModifiersState;

    fn page(w_pt: f64, h_pt: f64) -> PageImage {
        let (pw, ph) = (w_pt.max(1.0) as u32, h_pt.max(1.0) as u32);
        PageImage {
            rgba: Arc::from(vec![255u8; (pw * ph * 4) as usize]),
            width: pw,
            height: ph,
            width_pt: w_pt,
            height_pt: h_pt,
        }
    }

    /// One page-point-space line of `chars` starting at `x0`, each cell `cw`
    /// wide, baseline box `[y, y+12]`.
    fn line_run(y: f64, chars: &str, x0: f64, cw: f64) -> TextRun {
        let mut cells = Vec::new();
        let mut x = x0;
        for ch in chars.chars() {
            cells.push(TextCell {
                x0: x,
                x1: x + cw,
                text: ch.to_string(),
            });
            x += cw;
        }
        TextRun {
            top: y,
            bottom: y + 12.0,
            cells,
        }
    }

    /// A 200×800pt single-page preview in a 400×300 pane with one text line.
    fn pane() -> PreviewPane {
        let mut p = PreviewPane::new();
        p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        let pages = vec![page(200.0, 800.0)];
        let runs = vec![line_run(40.0, "Hello world", 20.0, 8.0)];
        p.set_pages_with_text(&pages, &[runs]);
        p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
        p
    }

    #[test]
    fn preview_zooms_fits_and_switches_pages() {
        // Case `ctrl_wheel_zooms_toward_cursor`.
        {
            let mut p = pane();
            let z0 = p.viewport.zoom();
            let center = p.viewport.rect().center();
            // Ctrl + wheel-up zooms in.
            p.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, 1.0),
                &ModifiersState::CONTROL,
                center,
                1.0,
            );
            assert!(p.viewport.zoom() > z0, "ctrl+wheel-up zooms in");
            assert!(!p.fit_width, "an explicit zoom drops the fit-width intent");
        }
        // Case `initial_fit_width_keeps_the_document_title_at_the_top`.
        {
            let p = pane();
            assert_eq!(
                p.viewport.scroll_y(),
                0.0,
                "a first fit-width layout must not centre-scroll past the title"
            );
        }
        // Case `switching_pages_via_thumb_scrolls`.
        {
            let mut p = PreviewPane::new();
            p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            let pages = vec![page(200.0, 260.0), page(200.0, 260.0), page(200.0, 260.0)];
            p.set_pages_with_text(&pages, &[vec![], vec![], vec![]]);
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
        // Case `preview_word_selection_uses_shared_boundaries_and_drag_state`.
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
        // Case `drag_selects_text_and_copies`.
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
        // Case `select_all_covers_every_cell_and_copies_with_line_break`.
        {
            let mut p = PreviewPane::new();
            p.set_rect(Rect::new(0.0, 0.0, 400.0, 300.0));
            let pages = vec![page(200.0, 400.0)];
            let runs = vec![
                line_run(40.0, "ab", 20.0, 8.0),
                line_run(80.0, "cd", 20.0, 8.0),
            ];
            p.set_pages_with_text(&pages, &[runs]);
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
        // Case `plain_wheel_scrolls_when_zoomed_tall`.
        {
            let mut p = pane();
            // Even at fit-width a 200×800pt page overflows the 300px pane vertically.
            assert!(p.viewport.overflows_y(), "tall page overflows vertically");
            p.demo_set_view(1.6, 0.3);
            let y0 = p.viewport.scroll_y();
            // Plain wheel-down pans further down.
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
        // Case `tilt_wheel_scrolls_vertically_when_no_horizontal_overflow`.
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
        // Case `drag_pans_when_zoomed_past_fit`.
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
        // Case `tilt_wheel_pans_horizontally_when_zoomed_past_fit`.
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
