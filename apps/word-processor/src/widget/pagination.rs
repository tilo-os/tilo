//! Pagination: flowing blocks, tables and images onto pages, and the page
//! geometry it works in.

use super::block_layout::{
    LIST_INDENT_STEP, align as block_align, bullet_glyph, cell_rc, cell_table, font as parse_font,
    indent as block_indent_attr, line_spacing as block_line_spacing, list_level,
    page_break_before as block_page_break_before, style_align, style_key as block_style_key,
};
use super::image_layout::{
    anchor as image_anchor, explicit_size as image_explicit_size, floats as image_floats,
    free_position as image_free_position, wrap as image_wrap,
};
use super::layout_cache::{ShapeKeyInput, shape_key};
use super::text_runs::{block_runs, composed_block};
use crate::num;
use std::collections::HashMap;
use tilo_doc::snapshot::BlockView;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_edit::EditCore;
use ui::text_render::TextRenderer;
use vello::kurbo::Rect;
use vello::peniko::Color;

use super::{
    ESTIMATED_BLOCK_EXTENT, Edges, FLOAT_GUTTER, FloatReserve, FrameGeometry, ImagePlacement, Laid,
    MAX_PAGE_W, PAGE_GAP, PAGE_TOP, PAPER_INK, PageCursor, PaginationResult,
    ParagraphPaginationContext, ParagraphPlacement, ParagraphShape, ResolvedImage,
    TableLayoutContext, TableRow, VIEWPORT_BLOCK_WINDOW, VIRTUALIZE_BODY_AFTER,
    WordProcessorWidget, layout_line_tops,
};

/// Frame-level geometry resolved once per frame by
/// [`WordProcessorWidget::draw_frame_chrome`] and threaded through `draw`'s
/// other per-pass helpers below — split out so `draw` itself stays under the
/// line-count limit.
/// The canvas clip as a vertical band in the coordinates of a laid text item
/// whose top is at `ly` (before the canvas transform), with a margin for
/// decorations drawn just outside their line box. Decorations of lines outside
/// the band cannot reach the clip. A degenerate transform yields NaN bounds,
/// which cull nothing.
pub(super) fn layout_clip_band(fg: &FrameGeometry, ly: f64) -> (f32, f32) {
    let local = fg.ctf.inverse().transform_rect_bbox(fg.canvas_clip);
    ((local.y0 - ly - 8.0) as f32, (local.y1 - ly + 8.0) as f32)
}

/// The spans that can intersect the visible source range (see
/// `RichTextLayout::source_range_within`); the others have no rectangle in
/// the band. Order is preserved.
pub(super) fn spans_within(
    spans: &[(usize, usize)],
    visible: Option<(usize, usize)>,
) -> Vec<(usize, usize)> {
    match visible {
        Some((start, end)) => spans
            .iter()
            .copied()
            .filter(|&(span_start, span_end)| span_end >= start && span_start <= end)
            .collect(),
        None => Vec::new(),
    }
}

/// The top-left content y for page `i`, in unscaled content space.
fn page_content_top(fg: &FrameGeometry, i: usize) -> f64 {
    fg.page_top + num::f64_of_usize(i) * fg.page_pitch + fg.m.top
}

/// The bottom text-margin y for page `i`, in unscaled content space.
fn page_content_bottom(fg: &FrameGeometry, i: usize) -> f64 {
    fg.page_top + num::f64_of_usize(i) * fg.page_pitch + fg.letter_h - fg.m.bottom
}

/// Map estimated continuous body flow into the page stack, including margins
/// and inter-page gaps. Window selection and initial pagination must use the
/// same coordinate system or their error grows with every preceding page.
pub(super) fn flow_to_page_stack(flow: f64, letter_h: f64, margins: Edges) -> f64 {
    let band_h = (letter_h - margins.top - margins.bottom).max(1.0);
    let flow = flow.max(0.0);
    (flow / band_h).floor() * (letter_h + PAGE_GAP) + margins.top + flow % band_h
}

/// Page margins and gaps map to the nearest body-flow boundary.
pub(super) fn page_stack_to_flow(stack: f64, letter_h: f64, margins: Edges) -> f64 {
    let band_h = (letter_h - margins.top - margins.bottom).max(1.0);
    let pitch = letter_h + PAGE_GAP;
    let stack = stack.max(0.0);
    (stack / pitch).floor() * band_h + (stack % pitch - margins.top).clamp(0.0, band_h)
}

/// How a page `page_w` wide shows the document's margins: in proportion
/// on a narrow or enlarged editing sheet, as they are on a print page.
pub(super) fn margin_scale(page_w: f64, mobile: bool, accessible: bool) -> f64 {
    if mobile || accessible {
        (page_w / MAX_PAGE_W).min(1.0)
    } else {
        1.0
    }
}

pub(super) fn responsive_page_geometry(
    canvas_left: f64,
    canvas_right: f64,
    text_scale: f64,
    margins: Edges,
    mobile: bool,
    accessible: bool,
) -> (f64, Edges) {
    let page_w = if accessible {
        ((canvas_right - canvas_left - 24.0) / text_scale).max(120.0)
    } else if mobile {
        (canvas_right - canvas_left - 24.0).max(240.0)
    } else {
        MAX_PAGE_W
    };
    if !mobile && !accessible {
        return (page_w, margins);
    }
    let scale = margin_scale(page_w, mobile, accessible);
    (
        page_w,
        Edges {
            top: margins.top * scale,
            right: margins.right * scale,
            bottom: margins.bottom * scale,
            left: margins.left * scale,
        },
    )
}

impl WordProcessorWidget {
    /// Pass 1 of [`Self::draw`]: shape blocks top-to-bottom and paginate them
    /// across discrete page sheets. Runs of consecutive `cell` blocks sharing a
    /// table id lay out as a grid; everything else stacks normally.
    pub(super) fn paginate_blocks(
        &mut self,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
    ) -> PaginationResult {
        let virtualized = self.body_order.len() > VIRTUALIZE_BODY_AFTER;
        let mut blocks = std::mem::take(&mut self.blocks);
        // An input-method composition is shaped inside its paragraph, so the
        // text after the caret moves aside and wraps with it. The block is
        // swapped for its composed view only while it is laid out.
        let composing = self.compose_caret_block(&mut blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        // Keep the current layouts separate from a bounded recently visited
        // window. Returning to it can reuse shaping, while the canonical key
        // below still checks text, marks, style and wrapping before reuse.
        let mut old_cache = std::mem::take(&mut self.layout_cache);
        let previous_current: std::collections::HashSet<String> = if virtualized {
            old_cache.keys().cloned().collect()
        } else {
            std::collections::HashSet::new()
        };
        for (id, layout) in std::mem::take(&mut self.recent_layout_cache) {
            old_cache.entry(id).or_insert(layout);
        }
        let mut new_cache: HashMap<String, (u64, RichTextLayout)> =
            HashMap::with_capacity(blocks.len());
        self.dbg_layout_hits = 0;
        self.dbg_layout_misses = 0;
        let cell_pad = 8.0f64;
        // Pagination cursor: current page index, whether anything has landed on
        // it yet (the first item on a page never breaks, so an over-tall block
        // simply overflows its own page — acceptable for v1), the running
        // content y, the currently-active floated image (subsequent paragraphs
        // on its page are narrowed to flow beside it — coarse wrap, see
        // `FloatReserve`), and the per-level ordered-list counters.
        let band_h = (page_content_bottom(fg, 0) - page_content_top(fg, 0)).max(1.0);
        let estimated_before = if virtualized {
            num::f64_of_usize(self.body_window_start) * ESTIMATED_BLOCK_EXTENT
        } else {
            0.0
        };
        let initial_page = num::usize_of_f64((estimated_before / band_h).floor());
        let initial_y = fg.page_top + flow_to_page_stack(estimated_before, fg.letter_h, fg.m);
        let mut cur = PageCursor {
            cur_page: initial_page,
            max_object_page: 0,
            placed_on_page: self.body_window_start > 0,
            y: initial_y,
            active_float: None,
            ol_counts: Vec::new(),
        };
        let mut bi = 0;
        while let Some(b) = blocks.get(bi) {
            // Coarse-wrap only narrows *paragraph* blocks beside a float. A table or
            // another image cannot share the float column, so before laying one out
            // push the cursor below the active float and retire it — they stack under
            // the image as a full-width block rather than overlapping it.
            let next_is_paragraph = !b.kind.eq("image") && cell_table(b).is_none();
            if !next_is_paragraph
                && let Some(f) = cur.active_float.take()
                && f.page == cur.cur_page
            {
                cur.y = cur
                    .y
                    .max(f.bottom + f64::from(self.sheet.get("body").space_after));
            }
            if cell_table(b).is_some() {
                let mut table_layout = TableLayoutContext {
                    cell_padding: cell_pad,
                    text_renderer,
                    old_cache: &mut old_cache,
                    new_cache: &mut new_cache,
                };
                bi = self.paginate_table(&blocks, bi, fg, &mut table_layout, &mut laid, &mut cur);
            } else if b.kind == "image" {
                self.paginate_image(b, fg, text_renderer, &mut new_cache, &mut laid, &mut cur);
                bi += 1;
            } else {
                let mut paragraph = ParagraphPaginationContext {
                    text_renderer,
                    old_cache: &mut old_cache,
                    new_cache: &mut new_cache,
                    laid: &mut laid,
                    cursor: &mut cur,
                };
                self.paginate_paragraph(b, fg, &mut paragraph);
                bi += 1;
            }
        }

        // A virtual window only shapes the viewport band. Its tail still needs
        // a stable estimated page extent for scrollbar geometry, but it must not
        // cause a full-document render or cache allocation.
        let estimated_last_page = if virtualized {
            num::usize_of_f64(
                ((num::f64_of_usize(self.body_order.len()) * ESTIMATED_BLOCK_EXTENT) / band_h)
                    .floor(),
            )
        } else {
            0
        };
        if virtualized {
            let mut unused: Vec<_> = old_cache
                .into_iter()
                .filter(|(id, _)| !new_cache.contains_key(id))
                .collect();
            // Prefer the immediately previous viewport over older history.
            unused.sort_by_key(|(id, _)| !previous_current.contains(id));
            self.recent_layout_cache = unused.into_iter().take(VIEWPORT_BLOCK_WINDOW).collect();
        }
        if let Some(composing) = composing {
            Self::restore_composed_block(&mut laid, &mut blocks, composing);
        }
        PaginationResult {
            laid,
            cache: new_cache,
            cur_page: cur
                .cur_page
                .max(cur.max_object_page)
                .max(estimated_last_page),
            y: cur.y,
            blocks,
        }
    }

    /// Swap the caret block in `blocks` for its view with the input method's
    /// composition laid in, returning its index, the committed block and the
    /// composition; `None` (and `blocks` untouched) while nothing composes.
    fn compose_caret_block(
        &self,
        blocks: &mut [BlockView],
    ) -> Option<(usize, BlockView, EditCore)> {
        if self.inner.in_running_region() {
            return None;
        }
        let index = blocks.iter().position(|b| b.id == self.inner.caret.block)?;
        let block = blocks.get_mut(index)?;
        let core = self.inner.composition(&block.text)?;
        let composed = composed_block(block, &core);
        Some((index, std::mem::replace(block, composed), core))
    }

    /// Put the committed block taken by [`Self::compose_caret_block`] back,
    /// and record on its laid slices the committed text and the composition
    /// their shaped layout shows.
    fn restore_composed_block(
        laid: &mut [Laid],
        blocks: &mut [BlockView],
        (index, committed, core): (usize, BlockView, EditCore),
    ) {
        for l in laid.iter_mut().filter(|l| l.id == committed.id) {
            l.text.clone_from(&committed.text);
            l.composition = Some(core.clone());
        }
        if let Some(block) = blocks.get_mut(index) {
            *block = committed;
        }
    }

    /// One table (a run of consecutive `cell` blocks sharing a table id):
    /// measure every row first (so the whole table paginates as one unit,
    /// never split mid-row across a page boundary), then lay out row by row,
    /// breaking to a fresh page when a row would cross the bottom margin — the
    /// first row on a page never breaks, so an over-tall single row overflows
    /// its page (acceptable for v1). Returns the block index just past the
    /// table's last cell.
    fn paginate_table(
        &mut self,
        blocks: &[BlockView],
        bi: usize,
        fg: &FrameGeometry,
        layout: &mut TableLayoutContext<'_>,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
    ) -> usize {
        let Some(tid) = blocks.get(bi).and_then(cell_table) else {
            // The caller only calls this when `cell_table(b).is_some()`; if
            // that ever changes (or `bi` is out of range), skip this one
            // block rather than losing the pagination cursor.
            return bi + 1;
        };
        let mut bi = bi;
        cur.ol_counts.clear();
        let start = bi;
        while blocks.get(bi).and_then(cell_table).as_deref() == Some(tid.as_str()) {
            bi += 1;
        }
        let cells = blocks.get(start..bi).unwrap_or(&[]);
        let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
        let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
        let col_w = f64::from(fg.content_w) / num::f64_of_usize(ncols);
        // Measure every row first so the whole table can paginate as one
        // unit (it is never split mid-row across a page boundary).
        let rows = self.measure_table_rows(cells, ncols, nrows, col_w, layout);
        let table_h: f64 = rows.iter().map(|(h, _)| *h).sum();
        // Break to the next page if a manual page break is set on the
        // first cell, or the whole table would not fit.
        let force_break = cells.first().is_some_and(block_page_break_before);
        let item_top = if cur.placed_on_page
            && (force_break || cur.y + table_h > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            cur.y
        };
        cur.placed_on_page = true;
        let mut ry = item_top;
        // Whether the current page already carries a row of this table —
        // the first row on a page never breaks (so an over-tall single row
        // overflows its page, acceptable for v1).
        let mut row_on_page = false;
        for (r, (row_h, row_items)) in rows.into_iter().enumerate() {
            // Split the table at a row boundary: if this whole row would
            // cross the page's bottom margin, move it to the next page's top
            // margin (rows that fit stay on this page). No header repeat.
            if row_on_page && ry + row_h > page_content_bottom(fg, cur.cur_page) + 0.5 {
                cur.cur_page += 1;
                ry = page_content_top(fg, cur.cur_page);
            }
            row_on_page = true;
            for (c, cell_h, cell) in row_items {
                let cell_x = fg.content_x + num::f64_of_usize(c) * col_w;
                let cbox = Rect::new(cell_x, ry, cell_x + col_w, ry + row_h);
                laid.push(Laid {
                    id: cell.id.clone(),
                    text: cell.text.clone(),
                    kind: "cell".into(),
                    size: 16.0,
                    y_top: ry + layout.cell_padding,
                    text_x: cell_x + layout.cell_padding,
                    width: col_w - 2.0 * layout.cell_padding,
                    height: cell_h,
                    layout_y: ry + layout.cell_padding,
                    clip: None,
                    bullet: None,
                    ink: PAPER_INK,
                    image_hash: None,
                    cell_box: Some(cbox),
                    cell_row: Some(r),
                    page: cur.cur_page,
                    composition: None,
                });
            }
            ry += row_h;
        }
        cur.y = ry + f64::from(self.sheet.get("body").space_after);
        bi
    }

    /// Measure every row of a table before laying it out, so the whole table
    /// can paginate as one unit (it is never split mid-row across a page
    /// boundary): the shaped (or cache-reused) layout for each populated
    /// cell, and each row's height (the tallest cell, or a minimum).
    fn measure_table_rows<'a>(
        &mut self,
        cells: &'a [BlockView],
        ncols: usize,
        nrows: usize,
        col_w: f64,
        layout: &mut TableLayoutContext<'_>,
    ) -> Vec<TableRow<'a>> {
        let mut rows: Vec<TableRow<'a>> = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut row_items: Vec<(usize, f64, &BlockView)> = Vec::new();
            let mut row_h = 26.0f64;
            for c in 0..ncols {
                if let Some(cell) = cells.iter().find(|cl| cell_rc(cl) == (r, c)) {
                    let (runs, underlines, strikes) = block_runs(cell, self.review.is_visible());
                    let wrap_w = num::f32_of_f64((col_w - 2.0 * layout.cell_padding).max(24.0));
                    let align = block_align(cell, TextAlign::Left);
                    let key = shape_key(&ShapeKeyInput {
                        spans: &runs,
                        underlines: &underlines,
                        strikes: &strikes,
                        base_bold: false,
                        size: 16.0,
                        wrap_w,
                        align,
                        family: FontFamily::Sans,
                        line_height_mult: 1.0,
                    });
                    let cell_layout = match layout.old_cache.remove(&cell.id) {
                        Some((k, lay)) if k == key => {
                            self.dbg_layout_hits += 1;
                            lay
                        }
                        _ => {
                            self.dbg_layout_misses += 1;
                            RichTextLayout::shape_styled(
                                &mut layout.text_renderer.font_system,
                                &runs,
                                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                                Some(wrap_w),
                                align,
                                underlines,
                                strikes,
                            )
                        }
                    };
                    let cell_h = f64::from(cell_layout.height());
                    row_h = row_h.max(cell_h + 2.0 * layout.cell_padding);
                    layout.new_cache.insert(cell.id.clone(), (key, cell_layout));
                    row_items.push((c, cell_h, cell));
                }
            }
            rows.push((row_h, row_items));
        }
        rows
    }

    /// Resolve one image block's decoded intrinsic size and the on-screen
    /// display size to draw it at: an explicit `width`/`height` attr (set by
    /// importers or a resize handle) wins; otherwise the decoded intrinsic
    /// size is fit to the content column. Either way the final width is
    /// clamped to the content column and the height follows the chosen
    /// aspect ratio, so a requested-but-too-wide image scales down without
    /// distorting. Also seeds `new_cache` with the block's trivial empty
    /// layout (used only so pointer hit-testing resolves to offset 0; cheap
    /// enough to re-shape every frame rather than cache incrementally).
    fn resolve_image_display_size(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
    ) -> ResolvedImage {
        let hash = b.str_attr("blob").unwrap_or_default().to_owned();
        if !hash.is_empty() && !self.image_cache.contains_key(&hash) {
            let decoded = self
                .blobs
                .get(&hash)
                .and_then(|bytes| ui::widget::Image::from_encoded(bytes).ok());
            self.image_cache.insert(hash.clone(), decoded);
        }
        let intrinsic = self
            .image_cache
            .get(&hash)
            .and_then(|o| o.as_ref())
            .map(ui::widget::Image::intrinsic_size);
        let (draw_w, draw_h) = match image_explicit_size(b) {
            Some((ew, eh)) if ew > 0.0 && eh > 0.0 => {
                let w = ew.min(f64::from(fg.content_w));
                (w, (eh * (w / ew)).max(8.0))
            }
            _ => match intrinsic {
                Some((iw, ih)) if iw > 0 && ih > 0 => {
                    let w = f64::from(iw).min(f64::from(fg.content_w));
                    (w, (f64::from(ih) * (w / f64::from(iw))).max(8.0))
                }
                _ => (f64::from(fg.content_w).min(280.0), 90.0),
            },
        };
        // Image blocks carry a trivial empty layout (used only so pointer
        // hit-testing resolves to offset 0); it is cheap, so just re-shape
        // it every frame rather than caching it incrementally.
        let empty = RichTextLayout::shape_styled(
            &mut text_renderer.font_system,
            &[RichSpan::new("")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(fg.content_w),
            TextAlign::Left,
            vec![],
            vec![],
        );
        new_cache.insert(b.id.clone(), (0, empty));
        ResolvedImage {
            hash,
            intrinsic,
            width: draw_w,
            height: draw_h,
        }
    }

    fn push_laid_image(
        laid: &mut Vec<Laid>,
        block: &BlockView,
        image: &ResolvedImage,
        placement: &ImagePlacement,
    ) {
        laid.push(Laid {
            id: block.id.clone(),
            text: String::new(),
            kind: "image".into(),
            size: 16.0,
            y_top: placement.y,
            text_x: placement.x,
            width: image.width,
            height: image.height,
            layout_y: placement.y,
            clip: None,
            bullet: None,
            ink: PAPER_INK,
            image_hash: image.intrinsic.map(|_| image.hash.clone()),
            cell_box: None,
            cell_row: None,
            page: placement.page,
            composition: None,
        });
    }

    /// One image block: resolve its decoded size, paginate it (a floated
    /// image reserves a column beside it instead of a full-width slot; see
    /// `FloatReserve`), and push its `Laid` entry.
    fn paginate_image(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
    ) {
        cur.ol_counts.clear();
        let image = self.resolve_image_display_size(b, fg, text_renderer, new_cache);
        // A user-positioned page object is intentionally out of normal flow:
        // text retains its natural cursor while the object is painted at its
        // durable page-relative coordinates. Imported `anchor=page` objects
        // without those coordinates keep the legacy centred-flow treatment
        // below, avoiding a layout change merely from opening an old file.
        let transient_position = self
            .object_drag
            .as_ref()
            .and_then(|drag| (drag.id == b.id).then_some((drag.x, drag.y, drag.page)));
        if image_anchor(b) == "page"
            && let Some((offset_x, offset_y, requested_page)) =
                transient_position.or_else(|| image_free_position(b))
        {
            let page = requested_page;
            let max_x = (f64::from(fg.content_w) - image.width).max(0.0);
            let max_y = (fg.letter_h - fg.m.top - fg.m.bottom - image.height).max(0.0);
            let img_x = fg.content_x + offset_x.clamp(0.0, max_x);
            let img_y = page_content_top(fg, page) + offset_y.clamp(0.0, max_y);
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: img_y,
                    page,
                },
            );
            // Keep enough sheets alive to show a deliberately later placement,
            // but leave `y` untouched so a free object cannot create phantom
            // blank paragraphs in the document flow.
            cur.max_object_page = cur.max_object_page.max(page);
            return;
        }
        // Break to the next page on a manual page break, or if the image
        // would not fit.
        let force_break = block_page_break_before(b);
        let item_top = if cur.placed_on_page
            && (force_break || cur.y + image.height > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            cur.y
        };
        cur.placed_on_page = true;
        if image_floats(b) {
            // Floated image (anchor=paragraph/page, wrap=left/right): instead
            // of a full-width vertical slot, pin it to the left or right edge
            // of the content column and reserve that column so following
            // paragraphs flow beside it (coarse wrap — `FloatReserve`). `cur.y`
            // does NOT advance past the image: the next blocks start level
            // with its top and are narrowed until they clear its bottom.
            let left = image_wrap(b) == "left";
            let img_x = if left {
                fg.content_x
            } else {
                fg.content_x + f64::from(fg.content_w) - image.width
            };
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: item_top,
                    page: cur.cur_page,
                },
            );
            cur.active_float = Some(FloatReserve {
                left,
                reserve_w: image.width + FLOAT_GUTTER,
                bottom: item_top + image.height,
                page: cur.cur_page,
            });
            // Leave `cur.y` at the image top so following text wraps beside it.
            cur.y = item_top;
        } else {
            // Inline (default), or anchored with wrap none/both: a full-width
            // vertical slot. A non-inline-anchored image with no one-sided
            // wrap is centered in the column; inline images stay left-aligned.
            let centered = image_anchor(b) != "inline";
            let img_x = if centered {
                fg.content_x + ((f64::from(fg.content_w) - image.width) / 2.0).max(0.0)
            } else {
                fg.content_x
            };
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: item_top,
                    page: cur.cur_page,
                },
            );
            cur.y = item_top + image.height + f64::from(self.sheet.get("body").space_after);
        }
    }

    /// Resolve one paragraph-like block's style, list indent/bullet, coarse
    /// float-wrap reserve, and shaped (or cache-reused) layout — everything
    /// [`Self::paginate_paragraph`] needs before it can decide where the
    /// block lands.
    fn resolve_paragraph_shape(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        old_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        cur: &mut PageCursor,
    ) -> ParagraphShape {
        // Resolve the block's style (component-instance model). `size`
        // and `base_bold` are the style default; per-run `size` marks
        // override them in `block_runs`.
        let st = self.sheet.get(&block_style_key(b));
        let (size, base_bold) = (st.size, st.bold);
        // List items indent by nesting level; the bullet/number sits just
        // left of the text. Ordered items number per level.
        let (indent, bullet) = if b.kind == "list_item" {
            let level = list_level(b);
            let indent = f64::from(st.indent) + num::f64_of_usize(level) * LIST_INDENT_STEP;
            // Keep the ordered counters sized to the current depth; entering
            // a shallower level drops the deeper counters so they restart.
            cur.ol_counts.truncate(level + 1);
            if cur.ol_counts.len() <= level {
                cur.ol_counts.resize(level + 1, 0);
            }
            let bullet = if b.flag("ordered") {
                let n = cur.ol_counts.get_mut(level).map_or(0, |c| {
                    *c += 1;
                    *c
                });
                format!("{n}.")
            } else {
                if let Some(c) = cur.ol_counts.get_mut(level) {
                    *c = 0; // a bullet item resets this level's number
                }
                bullet_glyph(level).to_string()
            };
            (indent, Some(bullet))
        } else {
            cur.ol_counts.clear();
            // Style indent + the paragraph's own ruler indent attr.
            (f64::from(st.indent) + block_indent_attr(b), None)
        };
        // Coarse float-wrap: if a floated image is active on this page and
        // this block's top still falls within the image band, narrow the
        // text column by the reserved width and (for a left float) push the
        // text origin right so it sits beside the image. The block is shaped
        // at the reduced width, so all its line geometry already reflects the
        // narrowing — draw + hit-test only need the shared x-origin offset.
        let (float_reserve, float_offset) = match cur.active_float {
            Some(f)
                if f.page == cur.cur_page
                    && cur.y + f64::from(st.space_before) + 0.5 < f.bottom =>
            {
                // Subtract the reserved column from the wrap width; for a
                // left float, shift the text origin right by the same amount.
                (f.reserve_w, if f.left { f.reserve_w } else { 0.0 })
            }
            _ => (0.0, 0.0),
        };
        let wrap_w =
            (fg.content_w - num::f32_of_f64(indent) - num::f32_of_f64(float_reserve)).max(40.0);
        let (runs, underlines, strikes) = block_runs(b, self.review.is_visible());
        let align = block_align(b, style_align(&st.align));
        let family = parse_font(&st.font);
        // The paragraph's line-spacing multiplier (1.0 = single) scales the
        // shaped line advance, so it is part of the shape key.
        let line_mult = block_line_spacing(b);
        let key = shape_key(&ShapeKeyInput {
            spans: &runs,
            underlines: &underlines,
            strikes: &strikes,
            base_bold,
            size,
            wrap_w,
            align,
            family,
            line_height_mult: line_mult,
        });
        // Reuse the shaped layout if nothing affecting shaping changed
        // (text, marks, resolved style, wrap width, or line spacing), else
        // re-shape.
        let layout = match old_cache.remove(&b.id) {
            Some((k, lay)) if k == key => {
                self.dbg_layout_hits += 1;
                lay
            }
            previous => {
                self.dbg_layout_misses += 1;
                // An edited block keeps the shaped lines it did not change
                // (its other paragraphs, or the untouched chunks of a huge
                // paragraph); any style or width change reshapes it all.
                RichTextLayout::shape_styled_reusing(
                    &mut text_renderer.font_system,
                    &runs,
                    &RichTextLayout::block_style(base_bold, size, family, line_mult),
                    Some(wrap_w),
                    align,
                    underlines,
                    strikes,
                    previous.map(|(_, layout)| layout),
                )
            }
        };
        ParagraphShape {
            st,
            indent,
            bullet,
            float_offset,
            wrap_w,
            key,
            layout,
        }
    }

    /// One paragraph-like block (a normal paragraph, heading, or list item):
    /// resolve its style/shape, decide whether it fits whole on the current
    /// page or needs to be split into per-page slices, and push its `Laid`
    /// entry/entries. Handles the paragraph side of float bookkeeping — a
    /// floated image only narrows blocks on its own page.
    fn paginate_paragraph(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        context: &mut ParagraphPaginationContext<'_>,
    ) {
        let ps = self.resolve_paragraph_shape(
            b,
            fg,
            context.text_renderer,
            context.old_cache,
            context.cursor,
        );
        let cur = &mut *context.cursor;
        let block_h = f64::from(ps.layout.height());
        // Place with the style's space-before as the leading gap, breaking
        // to the next page when the whole block would not fit before the
        // bottom text margin (the lead is dropped at the top of a fresh
        // page so the first paragraph hugs the top margin).
        let top_candidate = cur.y + f64::from(ps.st.space_before);
        // A manual page break forces a fresh page; otherwise break only
        // when the whole block would overflow the bottom text margin.
        let force_break = block_page_break_before(b);
        let item_top = if cur.placed_on_page
            && (force_break
                || top_candidate + block_h > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            top_candidate
        };
        cur.placed_on_page = true;
        // If the page advanced past the float (page-break), the float no
        // longer applies to this block — drop the x-origin offset so the
        // block lays full-width on its new page. (The float only ever
        // narrows blocks on its own page.)
        let float_offset = if cur.active_float.is_some_and(|f| f.page == cur.cur_page) {
            ps.float_offset
        } else {
            0.0
        };
        let pl = ParagraphPlacement {
            text_x: fg.content_x + ps.indent + float_offset,
            width: f64::from(ps.wrap_w),
            size: ps.st.size,
            bullet: ps.bullet.clone(),
            ink: Color::from_rgb8(ps.st.color[0], ps.st.color[1], ps.st.color[2]),
            space_after: ps.st.space_after,
        };
        // Does the whole block fit on the current page from `item_top`? If
        // so this is the fast, unchanged single-`Laid` path. Otherwise the
        // block is split at line boundaries across as many pages as needed.
        if item_top + block_h <= page_content_bottom(fg, cur.cur_page) + 0.5 {
            context.laid.push(Laid {
                id: b.id.clone(),
                text: b.text.clone(),
                kind: b.kind.clone(),
                size: pl.size,
                y_top: item_top,
                text_x: pl.text_x,
                width: pl.width,
                height: block_h,
                layout_y: item_top,
                clip: None,
                bullet: pl.bullet.clone(),
                ink: pl.ink,
                image_hash: None,
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
                composition: None,
            });
            cur.y = item_top + block_h + f64::from(pl.space_after);
        } else {
            Self::emit_paragraph_slices(b, fg, &ps.layout, context.laid, cur, item_top, &pl);
        }
        context.new_cache.insert(b.id.clone(), (ps.key, ps.layout));
        // Float bookkeeping: a float only narrows blocks on its own page and
        // only while text is still beside the image. Clear it once the page
        // has advanced past the float, or once this block has flowed down to
        // (or past) the image's bottom — and in that latter case make sure
        // the cursor sits at least at the image bottom so the next full-width
        // block clears the image vertically rather than overlapping it.
        if let Some(f) = cur.active_float {
            if cur.cur_page > f.page {
                cur.active_float = None;
            } else if cur.y >= f.bottom {
                cur.y = cur.y.max(f.bottom + f64::from(ps.st.space_after));
                cur.active_float = None;
            }
        }
    }

    /// Intra-block pagination for one paragraph-like block that doesn't fit
    /// on the current page: emit one `Laid` slice per page the block spans.
    /// Each slice shares the one cached `layout` (one id, one cache entry);
    /// only the *visual* rendering is split. A slice's `layout_y` maps the
    /// layout origin so line geometry (`selection_rects`/`caret_geometry`/
    /// `hit`) still resolves, and its `clip` band keeps lines from other
    /// slices off this page.
    fn emit_paragraph_slices(
        b: &BlockView,
        fg: &FrameGeometry,
        layout: &RichTextLayout,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
        item_top: f64,
        pl: &ParagraphPlacement,
    ) {
        // Intra-block pagination: emit one `Laid` slice per page the
        // block spans. Each slice shares the one cached layout (one id,
        // one cache entry); only the *visual* rendering is split. A
        // slice's `layout_y` maps the layout origin so that line geometry
        // (`selection_rects`/`caret_geometry`/`hit`) still resolves, and
        // its `clip` band keeps lines from other slices off this page.
        let lines = layout_line_tops(layout, &b.text);
        let mut li = 0usize;
        let mut slice_top = item_top; // on-screen top of the current slice
        let mut first_slice = true;
        while let Some(&(first_line_top, _)) = lines.get(li) {
            let bottom = page_content_bottom(fg, cur.cur_page);
            // layout_y maps line `li`'s internal top to `slice_top`.
            let layout_y = slice_top - first_line_top;
            let mut last = li;
            while let Some(&(nt, nh)) = lines.get(last + 1) {
                if layout_y + nt + nh > bottom + 0.5 {
                    break;
                }
                last += 1;
            }
            // Respect the shared paragraph widow/orphan policy. If the
            // next page would receive too few lines, leave enough there;
            // if this page cannot retain the minimum, move the paragraph
            // fragment wholesale instead of producing a one-line stub.
            let available = last - li + 1;
            let allowed = layout.fragment_line_count_from(li, available);
            if allowed == 0 && slice_top > page_content_top(fg, cur.cur_page) + 0.5 {
                cur.cur_page += 1;
                slice_top = page_content_top(fg, cur.cur_page);
                cur.y = slice_top;
                continue;
            }
            let allowed = allowed.max(1).min(available);
            last = li + allowed - 1;
            let Some(&(lt, lh)) = lines.get(last) else {
                // `last` is bounded above by the loop that grew it from
                // `li` (already a valid index) one lookahead at a time, so
                // this is unreachable; break defensively rather than panic.
                break;
            };
            let slice_bottom = layout_y + lt + lh;
            let band = Rect::new(pl.text_x, slice_top, pl.text_x + pl.width, slice_bottom);
            laid.push(Laid {
                id: b.id.clone(),
                text: b.text.clone(),
                kind: b.kind.clone(),
                size: pl.size,
                y_top: slice_top,
                text_x: pl.text_x,
                width: pl.width,
                height: slice_bottom - slice_top,
                layout_y,
                clip: Some(band),
                // The bullet/number belongs to the block's first line only.
                bullet: if first_slice { pl.bullet.clone() } else { None },
                ink: pl.ink,
                image_hash: None,
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
                composition: None,
            });
            li = last + 1;
            first_slice = false;
            if li < lines.len() {
                // More lines remain: spill onto the next page's top margin.
                cur.cur_page += 1;
                slice_top = page_content_top(fg, cur.cur_page);
                cur.y = slice_top;
            } else {
                cur.y = slice_bottom + f64::from(pl.space_after);
            }
        }
    }

    /// Pass 1's aftermath: the document's total extent, the clamped scroll
    /// offset, and shifting `laid` (+ its cell/clip rects) into screen space so
    /// hit-testing needs no further offset. Returns `(num_pages, sy)`.
    pub(super) fn finish_scroll(
        &mut self,
        fg: &FrameGeometry,
        laid: &mut [Laid],
        cur_page: usize,
        y: f64,
    ) -> (usize, f64) {
        // Total document extent below `fg.page_top`: the stacked page sheets + gaps,
        // widened if an over-tall block overflowed its page's text band.
        let num_pages = cur_page + 1;
        let pages_extent =
            num::f64_of_usize(num_pages) * fg.letter_h + num::f64_of_usize(cur_page) * PAGE_GAP;
        let extent = pages_extent.max(y - fg.page_top);

        // Scrolling: clamp the offset to the overflow, then shift the laid-out
        // content (+ pages) into screen space so hit-testing needs no offset.
        // `scroll_y` is an offset in unscaled content space; on screen the content
        // is magnified by `zoom`, so the visible window covers `fg.canvas.height() /
        // zoom` of content — divide by zoom so the overflow (and thus max scroll)
        // grows when zoomed in and shrinks when zoomed out.
        // What the status band says and what one key pages by.
        self.pages = num_pages;
        self.page_pitch = fg.page_pitch;
        self.scroll_viewport = fg.canvas.height() / self.zoom.factor();
        self.max_scroll =
            (extent + 2.0 * PAGE_TOP - fg.canvas.height() / self.zoom.factor()).max(0.0);
        self.follow_find_cover();
        self.scroll_y = self.scroll_y.clamp(self.scroll_floor(), self.max_scroll);
        let sy = self.scroll_y;
        if sy != 0.0 {
            for l in laid.iter_mut() {
                l.y_top -= sy;
                l.layout_y -= sy;
                if let Some(bx) = l.cell_box.as_mut() {
                    *bx = Rect::new(bx.x0, bx.y0 - sy, bx.x1, bx.y1 - sy);
                }
                if let Some(c) = l.clip.as_mut() {
                    *c = Rect::new(c.x0, c.y0 - sy, c.x1, c.y1 - sy);
                }
            }
        }

        (num_pages, sy)
    }
}
