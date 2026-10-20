//! Painting the laid-out document: the frame, page sheets, text, images,
//! change and note decorations, footnotes and other participants' overlays.

use super::block_layout::list_marker_x;
use super::image_layout::{
    crop_is_active as image_crop_is_active, cropped_source as cropped_image_source,
    paint_options as image_paint_options, paint_transform as image_paint_transform,
};
use super::text_runs::{
    block_change_decos, block_change_ranges, block_comment_ranges, block_note_refs, note_numbering,
    note_text,
};
use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::review_overlay::{DELETION_INK, INSERTION_INK, ReviewColors, ReviewOverlay};
use crate::ruler::Ruler;
use crate::text_offsets::char_to_byte;
use crate::zoom_control::MIN as ZOOM_MIN;
use std::collections::HashMap;
use tilo_doc::model::Region;
use tilo_doc::snapshot::BlockView;
use ui::PresenceCtx;
use ui::paint::with_alpha;
use ui::permissions::UserId;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use super::pagination::{layout_clip_band, responsive_page_geometry, spans_within};
use super::{
    BODY_DIM_VEIL_ALPHA, CARET_WIDTH, ChromeColors, FrameGeometry, LETTER_RATIO, Laid, LaidItemCtx,
    MAX_PAGE_W, NOTE_AREA_GAP, NOTE_CONTENT_PX, NOTE_INK, NOTE_SEP, NotesSpellcheckInput, PAGE_GAP,
    PAGE_TOP, PageContentInput, PageSheetInput, Pass2Inputs, Pass2Markers, RunningBand,
    UtilityBarsGeometry, WordProcessorWidget, draw_squiggle,
};

impl WordProcessorWidget {
    /// Chrome bands above the canvas — toolbar, Find bar,
    /// ruler — plus the canvas/page geometry they reserve around. The first
    /// pass of [`Self::draw`].
    pub(super) fn draw_frame_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) -> FrameGeometry {
        let tb_h = Self::toolbar_band_height(theme);
        self.layout_bar_height = Self::status_band_height(theme);
        let below = Rect::new(
            full.x0,
            full.y0 + tb_h,
            full.x1,
            (full.y1 - self.layout_bar_height).max(full.y0 + tb_h),
        );
        let column = self.resolve_drawers(theme, below);
        let available = column.width();
        let mobile_page = available < 600.0;
        let accessible_page = self.text_scale > 1.01;
        let page_right = self.page_area_right(theme, column, mobile_page);
        if self.view.auto_fit_page {
            // Phone-sized tiles use a responsive editing sheet instead of
            // shrinking an entire Letter page until its body copy is 7 px.
            // Wider canvases retain print-faithful page sizing and auto-fit.
            let fit = if accessible_page {
                self.text_scale
            } else if mobile_page {
                1.0
            } else {
                ((page_right - column.x0 - 32.0) / MAX_PAGE_W).clamp(ZOOM_MIN, 1.0)
            };
            self.zoom.set(fit);
        }
        let tb = Rect::new(full.x0, full.y0, full.x1, full.y0 + tb_h);
        self.tool_band = tb;
        self.draw_toolbar(scene, transform, text_renderer, theme, tb);

        // No bottom status strip: its readout floats as a transient component,
        // so the content spans down to the layout bar and
        // reclaims the strip's height. The layout bar now sits at the very bottom.
        let content_bottom = full.y1.max(full.y0 + tb_h);
        self.layout_bar_height = Self::status_band_height(theme);
        let layout_bar_top = (content_bottom - self.layout_bar_height).max(full.y0 + tb_h);
        // The page's column: what the docked outline and inspector leave.
        let canvas_left = column.x0;
        let canvas_right = column.x1.max(canvas_left + 1.0);
        // Reserve + draw the crash-recovery banner (when staged) and the Find
        // bar (when open), stacked directly under the toolbar. The find bar
        // is measured against the canvas width it will span first, so the
        // band it reserves (one row, or two when the controls would run off
        // the window) is the band it paints.
        let find_h = self.draw_find_bar(
            scene,
            transform,
            text_renderer,
            theme,
            &UtilityBarsGeometry {
                left: canvas_left,
                right: canvas_right,
                top: full.y0 + tb_h,
            },
        );
        // Reserve the horizontal ruler band (when on) out of the canvas top, below
        // the find bar. It is drawn with the *unscaled* `transform` so it stays a
        // fixed-height strip while the page scrolls/zooms; its tick + marker
        // positions are mapped through the zoom so they track the page edge.
        let ruler_h = if self.view.ruler_visible {
            Ruler::band_height(theme, text_renderer)
        } else {
            0.0
        };
        let canvas = Rect::new(
            canvas_left,
            full.y0 + tb_h + find_h + ruler_h,
            canvas_right,
            layout_bar_top,
        );
        self.zoom_viewport = canvas;

        // Zoom anchor: the canvas top, horizontally centered. The page+content
        // transform scales about this point, so the page stays centered and the
        // top of the (scrolled) content stays pinned to the canvas top as zoom
        // changes. The toolbar/find-bar/status bar are drawn with the unscaled
        // `transform` and are unaffected.
        self.zoom
            .set_anchor(Point::new(f64::midpoint(canvas.x0, page_right), canvas.y0));
        let ctf = self.content_transform(transform);

        // Per-side page geometry — every margin is independently customizable.
        // The page is a FIXED size (it does not resize with the window); the
        // window just shows more or less of it and centers it.
        let (page_w, m) = responsive_page_geometry(
            canvas_left,
            page_right,
            self.text_scale,
            self.margins,
            mobile_page,
            accessible_page,
        );
        // Centre in *content* space even when the page is wider than the
        // unscaled canvas. Scaling about the canvas centre then keeps both page
        // edges visible in auto-fit mode instead of pinning the left edge and
        // clipping the right.
        let page_x = f64::midpoint(canvas.x0, page_right) - page_w / 2.0;
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + m.left;
        let content_w = num::f32_of_f64((page_w - m.left - m.right).max(80.0));

        // Horizontal ruler band (drawn unscaled, above the canvas). Reads the
        // page edge geometry just computed, plus the caret paragraph's indent +
        // tab stops from the cached block views.
        if ruler_h > 0.0 {
            let band = Rect::new(
                canvas_left,
                full.y0 + tb_h + find_h,
                canvas_right,
                canvas.y0,
            );
            self.draw_ruler(scene, transform, text_renderer, theme, band, page_x, page_w);
        } else {
            self.ruler.clear();
        }
        // True pagination: each page is a fixed US-Letter sheet (height `letter_h`)
        // stacked with `PAGE_GAP` between sheets. A block lays into the current
        // page's text band [content_top, content_bottom]; when the *next whole
        // block* would not fit before the bottom text margin, the page advances and
        // the block starts at the next page's top margin — content is never split,
        // so a page-end never crosses a line of text.
        let letter_h = page_w * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        FrameGeometry {
            canvas,
            canvas_clip: transform.transform_rect_bbox(canvas),
            canvas_left,
            canvas_right,
            layout_bar_top,
            content_bottom,
            ctf,
            page_x,
            page_top,
            page_w,
            content_x,
            content_w,
            m,
            letter_h,
            page_pitch,
        }
    }

    /// The trailing edge of the area the page centres in. Review cards stand
    /// in a column beside the page: when a page centred in the whole
    /// `column` would leave too little room for them on its trailing side,
    /// the page moves over (and, fitted, shrinks) so the cards never cover
    /// the text. A phone-sized or large-text sheet keeps the whole width.
    fn page_area_right(&self, theme: &Theme, column: Rect, responsive: bool) -> f64 {
        if responsive || !self.review.shows_cards() {
            return column.x1;
        }
        let reserve = ReviewOverlay::column_reserve(theme);
        // An enlarged sheet reflows, and shares the canvas with review cards
        // unless that would leave less than a readable paragraph column.
        if self.text_scale > 1.01 {
            return if column.width() - reserve >= 320.0 * self.text_scale {
                column.x1 - reserve
            } else {
                column.x1
            };
        }
        let zoom = if self.view.auto_fit_page {
            ((column.width() - 32.0) / MAX_PAGE_W).clamp(ZOOM_MIN, 1.0)
        } else {
            self.zoom.factor()
        };
        let beside = (column.width() - MAX_PAGE_W * zoom) / 2.0;
        if beside >= reserve {
            column.x1
        } else {
            (column.x1 - reserve).max(column.x0 + 1.0)
        }
    }

    /// Push the canvas clip, then draw each discrete page sheet: shadow +
    /// paper, header/footer running bands (with the active edit-context band
    /// highlighted), and the default "Page N" indicator when there's no footer
    /// of its own. Returns the reference (first) page's rect in screen space.
    pub(super) fn draw_page_sheets(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
        num_pages: usize,
        sy: f64,
    ) -> Rect {
        // Clip the document to the canvas so scrolled content never paints over
        // the toolbar / status bar.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &fg.canvas,
        );

        // Discrete page sheets (shadow + paper) stacked with `PAGE_GAP`. Each
        // sheet draws the page header inside its top-margin band and the footer
        // inside its bottom-margin band (page-number fields resolved to this
        // page's number). A subtle "Page N" indicator lives in the gap below the
        // sheet only when the document has no footer of its own. All through
        // `fg.ctf` (the zoom transform) so the page magnifies with the content.
        let header = std::mem::take(&mut self.header);
        let footer = std::mem::take(&mut self.footer);
        // Which running region (if any) is currently the active edit context, and
        // on which page its editable band lives (the first page is the reference —
        // the header/footer content is shared across pages, like Word).
        let edit_region = self.inner.region();
        let caret_block = self.inner.caret.block.clone();
        let band_composition = header
            .iter()
            .chain(&footer)
            .find(|b| self.inner.in_running_region() && b.id == caret_block)
            .and_then(|b| self.inner.composition(&b.text));
        self.region_laid.clear();
        // Drawing every sheet makes a long imported document O(page_count) per
        // frame even though the canvas clips all but a handful. Paint only the
        // visible sheets plus one neighbour for the inter-page indicator.
        let visible_top = sy + fg.canvas.y0;
        let visible_bottom = sy + fg.canvas.y1 / self.zoom.factor();
        let first_page = num::usize_of_f64(
            ((visible_top - fg.page_top) / fg.page_pitch)
                .floor()
                .max(0.0),
        )
        .min(num_pages.saturating_sub(1));
        let last_page = num::usize_of_f64(
            ((visible_bottom - fg.page_top) / fg.page_pitch)
                .ceil()
                .max(0.0),
        )
        .saturating_add(1)
        .min(num_pages);
        for i in first_page..last_page {
            self.draw_page_sheet(
                scene,
                text_renderer,
                fg,
                &PageSheetInput {
                    header: &header,
                    footer: &footer,
                    composition: band_composition
                        .as_ref()
                        .map(|core| (caret_block.as_str(), core)),
                    edit_region,
                    index: i,
                    scroll_y: sy,
                },
            );
        }
        self.header = header;
        self.footer = footer;
        // The visible page is the reference rect for guides and hit-testing.
        // Margins apply identically to every sheet, so retaining page zero here
        // would make guides unusable after scrolling through a long document.
        let page = Rect::new(
            fg.page_x,
            fg.page_top + num::f64_of_usize(first_page) * fg.page_pitch - sy,
            fg.page_x + fg.page_w,
            fg.page_top + num::f64_of_usize(first_page) * fg.page_pitch + fg.letter_h - sy,
        );
        self.page_rect = page;
        page
    }

    /// One page sheet (shadow + paper), its header/footer running bands (with
    /// the active edit-context band highlighted), and the default "Page N"
    /// indicator when there's no footer of its own.
    fn draw_page_sheet(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
        input: &PageSheetInput<'_>,
    ) {
        let PageSheetInput {
            header,
            footer,
            composition,
            edit_region,
            index: i,
            scroll_y: sy,
        } = *input;
        let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
        let sheet = Rect::new(fg.page_x, ptop, fg.page_x + fg.page_w, ptop + fg.letter_h);
        // The sheet: lifted off the desk by the floating shadow, with an
        // edge derived from the paper so it separates from a light desk too.
        let radius = f64::from(self.paper_theme.tokens.radii.detail);
        ui::surface::shadow(scene, fg.ctf, sheet, radius, &self.chrome.paper_elevation);
        ui::surface::card(scene, fg.ctf, sheet, radius, self.chrome.paper);
        let page_no = i + 1;
        // Header band: the top margin, inset by the L/R text margins.
        let head_band = Rect::new(
            sheet.x0 + fg.m.left,
            sheet.y0,
            sheet.x1 - fg.m.right,
            sheet.y0 + fg.m.top,
        );
        // Footer band: the bottom margin, inset by the L/R text margins.
        let foot_band = Rect::new(
            sheet.x0 + fg.m.left,
            sheet.y1 - fg.m.bottom,
            sheet.x1 - fg.m.right,
            sheet.y1,
        );
        // Cache the reference-page band rects for the event handler's hit-test.
        if i == 0 {
            self.head_band_rect = head_band;
            self.foot_band_rect = foot_band;
        }
        let head_editing = i == 0 && edit_region == Region::Header;
        let foot_editing = i == 0 && edit_region == Region::Footer;
        let (_, head_laid) = Self::draw_running_band(
            scene,
            fg.ctf,
            text_renderer,
            &RunningBand {
                blocks: header,
                page_no,
                rect: head_band,
                is_footer: false,
                editing: head_editing,
                composition: composition.filter(|_| head_editing),
            },
        );
        let (drew_footer, foot_laid) = Self::draw_running_band(
            scene,
            fg.ctf,
            text_renderer,
            &RunningBand {
                blocks: footer,
                page_no,
                rect: foot_band,
                is_footer: true,
                editing: foot_editing,
                composition: composition.filter(|_| foot_editing),
            },
        );
        if head_editing {
            self.region_laid = head_laid;
            self.draw_active_band_chrome(scene, fg.ctf, text_renderer, head_band);
        }
        if foot_editing {
            self.region_laid = foot_laid;
            self.draw_active_band_chrome(scene, fg.ctf, text_renderer, foot_band);
        }
        // Default page indicator in the gap — only when no footer was drawn.
        if !drew_footer && !foot_editing {
            // Drawn in the inter-page gap on the desk: window ink.
            let label = t_args("page-indicator", &[arg("n", num::i64_of_usize(page_no))]);
            let style = self.desk_theme.text(TextRole::Caption).numeric();
            let (w, h) = text_renderer.measure_style(&label, None, &style);
            let lx = f64::midpoint(sheet.x0, sheet.x1) - f64::from(w) / 2.0;
            let ly = sheet.y1 + (PAGE_GAP - f64::from(h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &label,
                self.desk_theme.tokens.colors.text_dim,
                fg.ctf * Affine::translate((lx, ly)),
                None,
                &style,
            );
        }
    }

    /// Restore the block cache pass 1 took, then precompute pass 2's read-only
    /// inputs: whether the selection spans multiple cells, footnote citation
    /// numbers in document order, and (when spell-check is on) the misspelled
    /// spans for every body block near the viewport. Returns `(multi_sel,
    /// note_nums, spell_by_block)`.
    pub(super) fn prepare_pass2_inputs(
        &mut self,
        blocks: Vec<BlockView>,
        laid: &[Laid],
        fg: &FrameGeometry,
    ) -> Pass2Inputs {
        // Restore the block cache before pass 2 so selection lookups resolve.
        self.blocks = blocks;
        let multi_sel = self.selection_blocks_cached().len() > 1;

        // Footnote numbering: assign citation numbers (1,2,3…) to notes in the
        // order their `note-ref` markers first appear in the body. This drives
        // both the in-text superscript and the bottom-of-page footnote list.
        let note_nums = note_numbering(&self.blocks);

        // Spell-check (squiggles): precompute the misspelled byte spans for every
        // body block *near the viewport* before pass 2, so the read-only pass-2
        // loop (which borrows `self` immutably) can look them up without a `&mut`
        // borrow conflict. Cheap: cached per block keyed by text (an edit
        // re-checks), and culled to a band around the visible canvas.
        let mut spell_by_block: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        if self.spellcheck_enabled() {
            // A generous band around the canvas (one screenful above/below) so a
            // block scrolling into view is already checked; everything else is
            // skipped to keep the pass cheap on long documents.
            let cull_lo = fg.canvas.y0 - fg.canvas.height();
            let cull_hi = fg.canvas.y1 + fg.canvas.height();
            // Collect the (id, text) of in-band, spellable body blocks first to end
            // the immutable borrow of `laid`/`self.blocks` before the `&mut self`
            // dictionary/cache calls below.
            let targets: Vec<(String, String)> = laid
                .iter()
                .filter(|l| {
                    !l.text.is_empty()
                        && l.kind != "image"
                        && l.kind != "fidelity"
                        && l.y_top + l.height >= cull_lo
                        && l.y_top <= cull_hi
                })
                .map(|l| (l.id.clone(), l.text.clone()))
                .collect();
            for (id, text) in targets {
                let spans = self.block_spell_spans(&id, &text);
                if !spans.is_empty() {
                    spell_by_block.insert(id, spans);
                }
            }
        }

        (multi_sel, note_nums, spell_by_block)
    }

    /// Pass 2 of [`Self::draw`]: paint cell borders, bullets, selection, the
    /// shaped text, decorations (find/comment/tracked-change highlights,
    /// spell-check squiggles, footnote superscripts) and the caret for every
    /// laid item. Collects the footnote/comment/change-card bookkeeping that
    /// the passes after this one render.
    pub(super) fn draw_page_content(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        input: &PageContentInput<'_>,
    ) -> Pass2Markers {
        let PageContentInput {
            laid,
            cache: new_cache,
            multi_selection: multi_sel,
            spell_by_block,
            note_numbers: note_nums,
        } = *input;
        let mut page_notes: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut comment_markers: Vec<(String, f64, f64)> = Vec::new();
        let mut change_markers: Vec<(String, f64, f64)> = Vec::new();

        // Pass 2: draw cell borders, bullets, selection, text, caret. Everything
        // here renders through `fg.ctf` (the zoom transform) so page content scales
        // while the surrounding chrome stays at 100%.
        for l in laid {
            if l.kind == "image" {
                self.draw_laid_image(scene, fg, text_renderer, l);
                continue;
            }
            if let Some(bx) = l.cell_box {
                self.draw_laid_cell_box(scene, fg, bx, l.cell_row);
            }

            if l.kind == "fidelity" {
                Self::draw_laid_fidelity(
                    scene,
                    fg,
                    text_renderer,
                    l,
                    &self.chrome,
                    &self.paper_theme,
                );
                continue;
            }

            Self::draw_laid_bullet(scene, fg, text_renderer, l);

            // The shaped text lives in the layout cache (built in pass 1), keyed
            // by block id. Every normal/cell entry was inserted there.
            let Some((_, layout)) = new_cache.get(&l.id) else {
                continue;
            };
            // Layout-relative geometry is measured from `layout_y` (= the on-screen
            // y of the shaped layout's origin). For an unsplit block this equals
            // `y_top`; for a slice of a page-split block it is offset so the carried
            // lines land in this slice's band.
            let ly = l.layout_y;
            // A split slice clips its drawing to its own band so lines belonging to
            // the block's other slices (drawn from the same layout via `ly`) never
            // bleed onto this page. Unsplit blocks (`clip == None`) draw unclipped.
            let clipped = l.clip.is_some();
            if let Some(band) = l.clip {
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    fg.ctf,
                    &band,
                );
            }

            let item = LaidItemCtx { l, layout, ly };
            self.draw_laid_text(
                scene,
                fg,
                text_renderer,
                &item,
                multi_sel,
                &mut comment_markers,
            );
            self.draw_laid_change_decos(scene, fg, &item, &mut change_markers);
            self.draw_laid_notes_and_spellcheck(
                scene,
                fg,
                text_renderer,
                &item,
                NotesSpellcheckInput {
                    spell_by_block,
                    note_numbers: note_nums,
                    page_notes: &mut page_notes,
                },
            );

            if self.lifecycle.focused
                && l.id == self.inner.caret.block
                && !self.inner.has_selection()
            {
                let byte = l.caret_byte(self.inner.caret.offset);
                let (cx, cy, ch) = layout.caret_geometry(byte);
                let caret = Rect::new(
                    l.text_x + f64::from(cx),
                    ly + f64::from(cy),
                    l.text_x + f64::from(cx) + CARET_WIDTH,
                    ly + f64::from(cy) + f64::from(ch),
                );
                self.ime_rect = Some(fg.ctf.transform_rect_bbox(caret));
                scene.fill(
                    Fill::NonZero,
                    fg.ctf,
                    self.chrome.paper_accent,
                    None,
                    &caret,
                );
            }
            if clipped {
                scene.pop_layer();
            }
        }

        Pass2Markers {
            page_notes,
            comment_markers,
            change_markers,
        }
    }

    fn draw_laid_bullet(
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
    ) {
        if let Some(bul) = &l.bullet {
            // The marker column is right-aligned at the same gap for `9.`
            // and `10.` alike. A fixed start point makes multi-digit
            // counters grow into the paragraph and visibly collide at
            // large text. Measure with the exact font/scale used below,
            // then reserve the same perceived gap for every counter width.
            let marker = ui::text_render::TextStyle::content(l.size);
            let (marker_width, _) = text_renderer.measure_style(bul, None, &marker);
            let marker_x = list_marker_x(l.text_x, f64::from(marker_width));
            text_renderer.draw_style(
                scene,
                bul,
                l.ink,
                fg.ctf * Affine::translate((marker_x, l.y_top)),
                None,
                &marker,
            );
        }
    }

    /// A table cell's tinted background (header row / zebra stripe / plain)
    /// + border, for one pass-2 item.
    fn draw_laid_cell_box(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        bx: Rect,
        cell_row: Option<usize>,
    ) {
        // Document table styling on the sheet (not OS chrome): default header
        // tint, zebra and hairline the authored table carries.
        let fill = match cell_row {
            Some(0) => Color::from_rgb8(0xec, 0xee, 0xf3), // header row tint
            Some(r) if r % 2 == 0 => Color::from_rgb8(0xf6, 0xf6, 0xf9), // zebra
            _ => self.chrome.paper,
        };
        scene.fill(Fill::NonZero, fg.ctf, fill, None, &bx);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            fg.ctf,
            Color::from_rgb8(0xc4, 0xc6, 0xcc),
            None,
            &bx,
        );
    }

    /// One pass-2 image item: paint it (mirrored per `flip_h`/`flip_v`, or a
    /// placeholder if undecoded), then ring it with the selection chrome when
    /// it's the caret's block.
    fn draw_laid_image(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
    ) {
        let rect = Rect::new(l.text_x, l.y_top, l.text_x + l.width, l.y_top + l.height);
        // Mirror flips (`flip_h` / `flip_v` block attrs) are applied as a
        // scale-about-centre on the paint transform, so the image draws
        // mirrored in place without moving its slot.
        let options = image_paint_options(self.blocks.iter().find(|b| b.id == l.id));
        let image_transform = image_paint_transform(fg.ctf, rect, options);
        if let Some(hash) = &l.image_hash {
            if let Some(slot) = self.image_cache.get_mut(hash)
                && let Some(img) = slot
            {
                // Paint a scaled full-source rectangle through a clip matching
                // the stable display frame. This is source cropping, not a
                // resize: the frame and surrounding text layout never jump.
                let source = cropped_image_source(rect, options.crop);
                if image_crop_is_active(options.crop) {
                    scene.push_layer(
                        Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0,
                        fg.ctf,
                        &rect,
                    );
                    img.paint_at(scene, image_transform, source);
                    scene.pop_layer();
                } else {
                    img.paint_at(scene, image_transform, rect);
                }
            }
        } else {
            Self::draw_missing_image(
                scene,
                fg.ctf,
                text_renderer,
                rect,
                &self.chrome,
                &self.paper_theme,
            );
        }
        // Selection chrome: when this image is the caret's block, ring it
        // with an accent border + corner resize handles so it reads as
        // "selected" (the panel then shows the resize / flip controls).
        if l.id == self.inner.caret.block {
            Self::draw_selected_image_chrome(
                scene,
                fg.ctf,
                rect,
                self.chrome.paper_accent,
                self.chrome.paper,
            );
        }
    }

    fn draw_missing_image(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        rect: Rect,
        chrome: &ChromeColors,
        paper: &Theme,
    ) {
        let rounded =
            vello::kurbo::RoundedRect::from_rect(rect, f64::from(paper.tokens.radii.detail));
        scene.fill(Fill::NonZero, transform, chrome.paper_tint, None, &rounded);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            chrome.paper_edge,
            None,
            &rounded,
        );
        let label = t("canvas-image-missing");
        let style = paper.text(TextRole::Body);
        let (_, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            paper.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    rect.x0 + f64::from(paper.tokens.spacing.s3),
                    rect.center().y - f64::from(h) / 2.0,
                )),
            None,
            &style,
        );
    }

    fn draw_selected_image_chrome(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        accent: Color,
        handle_fill: Color,
    ) {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            accent,
            None,
            &rect,
        );
        let handle_radius = 5.0;
        for (x, y) in [
            (rect.x0, rect.y0),
            (rect.x1, rect.y0),
            (rect.x1, rect.y1),
            (rect.x0, rect.y1),
        ] {
            let handle = Rect::new(
                x - handle_radius,
                y - handle_radius,
                x + handle_radius,
                y + handle_radius,
            );
            scene.fill(Fill::NonZero, transform, handle_fill, None, &handle);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                accent,
                None,
                &handle,
            );
        }
    }

    /// One pass-2 "fidelity" (unrenderable embedded content) placeholder item.
    fn draw_laid_fidelity(
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
        chrome: &ChromeColors,
        paper: &Theme,
    ) {
        let box_rect = Rect::new(
            l.text_x,
            l.y_top + 2.0,
            (fg.page_x + fg.page_w - fg.m.right).min(l.text_x + 360.0),
            l.y_top + l.height - 2.0,
        );
        scene.fill(
            Fill::NonZero,
            fg.ctf,
            chrome.paper_tint,
            None,
            &vello::kurbo::RoundedRect::from_rect(box_rect, f64::from(paper.tokens.radii.detail)),
        );
        let label = t("canvas-embedded-content");
        let style = paper.text(TextRole::Body);
        let (_, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            paper.tokens.colors.text_dim,
            fg.ctf
                * Affine::translate((
                    box_rect.x0 + f64::from(paper.tokens.spacing.s3),
                    box_rect.center().y - f64::from(h) / 2.0,
                )),
            None,
            &style,
        );
    }

    /// Selection highlight, find-match highlight, comment highlight (which
    /// also records `comment_markers` for the review-card pass), and the
    /// shaped text itself, for one pass-2 item.
    fn draw_laid_text(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        item: &LaidItemCtx,
        multi_sel: bool,
        comment_markers: &mut Vec<(String, f64, f64)>,
    ) {
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        if let Some((b0, b1)) = self
            .selection_bytes_for(&l.id, &l.text)
            .map(|(b0, b1)| (l.shown(b0), l.shown(b1)))
        {
            if let (Some(bx), true) = (l.cell_box, multi_sel) {
                // Multi-cell selection: highlight the whole cell (spreadsheet-style).
                scene.fill(Fill::NonZero, fg.ctf, self.paper_selection(), None, &bx);
            } else {
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, self.paper_selection(), None, &r);
                }
            }
        }

        // Find-match highlights (under the text).
        if self.find.is_open() {
            let ranges: Vec<(usize, usize)> = self
                .find
                .matches()
                .iter()
                .filter(|(bid, _, _)| bid == &l.id)
                .map(|(_, cs, ce)| (l.layout_byte(*cs), l.layout_byte(*ce)))
                .collect();
            let current = self
                .find
                .active_match()
                .filter(|(bid, _, _)| bid == &l.id)
                .map(|(_, cs, ce)| (char_to_byte(&l.text, *cs), char_to_byte(&l.text, *ce)));
            let (top, bottom) = layout_clip_band(fg, ly);
            let ranges = spans_within(&ranges, layout.source_range_within(top, bottom));
            for (range, rects) in ranges
                .iter()
                .zip(layout.selection_rects_many_within(&ranges, top, bottom))
            {
                for (rx0, ry0, rx1, ry1) in rects {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    ui::widget::find_bar::paint_find_match(
                        scene,
                        fg.ctf,
                        r,
                        current == Some(*range),
                        &self.paper_theme,
                    );
                }
            }
        }
        // Comment highlight bands (under the text): a pale-yellow band over
        // each `comment`-marked range. Mapped to on-screen rects via the same
        // `selection_rects` geometry as the selection highlight, so it tracks
        // wrapping. The comment's start line is recorded for the right-margin
        // indicator/card rendered after the loop. Only when markup is shown.
        if self.review.is_visible()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            for (bs, be, cid) in block_comment_ranges(b) {
                let author = self.review.comment_author(&cid).unwrap_or("");
                let wash =
                    ReviewColors::comment_highlight(&self.paper_theme, self.chrome.paper, author);
                let rects = layout.selection_rects(l.shown(bs), l.shown(be));
                for &(rx0, ry0, rx1, ry1) in &rects {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, wash, None, &r);
                }
                // Anchor the marker at the start line's top, and remember
                // the range's right edge (max rect x1) for the inline tab.
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    comment_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }

        let text_transform = fg.ctf * Affine::translate((l.text_x, ly));
        #[cfg(test)]
        if self.reference_text_rendering {
            text_renderer.draw_rich(scene, layout, text_transform, l.ink);
            return;
        }
        // Keep the real canvas/slice layers and all paragraph bookkeeping.
        // Only omit glyph encoding whose ink cannot reach either clip.
        let clip = l.clip.map_or(fg.canvas_clip, |clip| {
            fg.canvas_clip.intersect(fg.ctf.transform_rect_bbox(clip))
        });
        text_renderer.draw_rich_in_clip(scene, layout, text_transform, l.ink, clip);
    }

    /// Tracked-change decorations (green underline for `ins`, red strike for
    /// `del`) over one pass-2 item's text, plus recording `change_markers`
    /// for the review-card pass. Drawn here (rather than via the shaped
    /// layout's own underline/strike lists) so the decoration colour matches
    /// the review-tinted glyphs — the toolkit paints shaped decorations in
    /// the block ink. Only when markup is shown.
    fn draw_laid_change_decos(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        item: &LaidItemCtx,
        change_markers: &mut Vec<(String, f64, f64)>,
    ) {
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        if self.review.is_visible()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            let (ins_ranges, del_ranges) = block_change_decos(b);
            for &(b0, b1) in &ins_ranges {
                for (rx0, _ry0, rx1, ry1) in layout.selection_rects(l.shown(b0), l.shown(b1)) {
                    let uy = ly + f64::from(ry1) - 2.0;
                    let line = Rect::new(
                        l.text_x + f64::from(rx0),
                        uy,
                        l.text_x + f64::from(rx1),
                        uy + 1.5,
                    );
                    scene.fill(Fill::NonZero, fg.ctf, INSERTION_INK, None, &line);
                }
            }
            for &(b0, b1) in &del_ranges {
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(l.shown(b0), l.shown(b1)) {
                    let my = ly + f64::from(f32::midpoint(ry0, ry1));
                    let line = Rect::new(
                        l.text_x + f64::from(rx0),
                        my,
                        l.text_x + f64::from(rx1),
                        my + 1.5,
                    );
                    scene.fill(Fill::NonZero, fg.ctf, DELETION_INK, None, &line);
                }
            }
            // Collect a review-card marker per change range (carrying the
            // change id so the card's Accept/Reject can act on it).
            for (bs, be, cid) in block_change_ranges(b) {
                let rects = layout.selection_rects(l.shown(bs), l.shown(be));
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    change_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }
    }

    /// Spell-check squiggles + note-reference superscripts over one pass-2
    /// item's text, recording `page_notes` for the footnote-area pass.
    fn draw_laid_notes_and_spellcheck(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        item: &LaidItemCtx,
        input: NotesSpellcheckInput<'_>,
    ) {
        let NotesSpellcheckInput {
            spell_by_block,
            note_numbers: note_nums,
            page_notes,
        } = input;
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        // Spell-check squiggles: a wavy red underline under each misspelled
        // span. The byte span maps to one-or-more on-screen rects via the same
        // `selection_rects` geometry the selection highlight uses (so it tracks
        // wrapping). The squiggle hugs just under each rect's bottom.
        if let Some(spans) = spell_by_block.get(&l.id) {
            // One pass over the paragraph's runs for all of its spans (a
            // pasted page can carry thousands of squiggles).
            let (top, bottom) = layout_clip_band(fg, ly);
            let shown: Vec<(usize, usize)>;
            let spans = if l.composition.is_some() {
                shown = spans
                    .iter()
                    .map(|&(s, e)| (l.shown(s), l.shown(e)))
                    .collect();
                &shown
            } else {
                spans
            };
            let ranges = spans_within(spans, layout.source_range_within(top, bottom));
            for rects in layout.selection_rects_many_within(&ranges, top, bottom) {
                for (rx0, _ry0, rx1, ry1) in rects {
                    let y = ly + f64::from(ry1) - 1.0;
                    draw_squiggle(
                        scene,
                        fg.ctf,
                        self.chrome.paper_danger,
                        l.text_x + f64::from(rx0),
                        l.text_x + f64::from(rx1),
                        y,
                    );
                }
            }
        }

        // Note-reference superscripts: each `note-ref` placeholder in this
        // block renders as a small superscript citation number, positioned at
        // the placeholder glyph's location (it occupies one U+FFFC char, which
        // the body run shapes as near-zero-width). The number is taken from the
        // document-order numbering; the page is recorded so the footnote text
        // renders at the bottom of this page.
        if !note_nums.is_empty()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            for (ci, note_id) in block_note_refs(b) {
                let Some(&num) = note_nums.get(&note_id) else {
                    continue;
                };
                let bs = l.layout_byte(ci);
                let be = l.layout_byte(ci + 1);
                // Anchor at the placeholder's left edge; fall back to the
                // caret geometry if the glyph produced no selection rect.
                let placeholder_rect = layout.selection_rects(bs, be).into_iter().next();
                let (mx, my) = placeholder_rect.map_or_else(
                    || {
                        let (cx, cy, _) = layout.caret_geometry(bs);
                        (cx, cy)
                    },
                    |(rx0, ry0, _, _)| (rx0, ry0),
                );
                // Mask the U+FFFC object-replacement glyph (a box in some
                // fonts) with paper so only the superscript number shows.
                if let Some((rx0, ry0, rx1, ry1)) = placeholder_rect {
                    let mask = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, self.chrome.paper, None, &mask);
                }
                let label = num.to_string();
                // Superscript: smaller font, raised toward the line's top.
                let sup_size = (l.size * 0.7).max(8.0);
                text_renderer.draw_style(
                    scene,
                    &label,
                    l.ink,
                    fg.ctf
                        * Affine::translate((l.text_x + f64::from(mx), ly + f64::from(my) - 1.0)),
                    None,
                    &ui::text_render::TextStyle::content(sup_size),
                );
                page_notes.entry(l.page).or_default().push(num);
            }
        }
    }

    /// Footnote areas: a thin separator + numbered footnotes at the bottom of
    /// each page's text band, keyed by the page pass 2 recorded each marker
    /// against. Page assignment is approximate; drawn inside the page clip so
    /// it scrolls with the content. Endnotes are skipped (they belong at the
    /// document end, not per page).
    pub(super) fn draw_footnotes(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        page_notes: &HashMap<usize, Vec<usize>>,
        note_nums: &HashMap<String, usize>,
        sy: f64,
    ) {
        if !page_notes.is_empty() {
            // number → footnote text, resolved once. Endnotes are skipped here
            // (they belong at the document end, not per page).
            let mut num_to_note: HashMap<usize, String> = HashMap::new();
            for n in &self.notes {
                if n.kind == tilo_doc::schema::NOTE_KIND_FOOTNOTE
                    && let Some(&num) = note_nums.get(&n.id)
                {
                    num_to_note.insert(num, note_text(n));
                }
            }
            for (page, nums) in page_notes {
                // Distinct, ascending footnote numbers on this page.
                let mut nums: Vec<usize> = nums
                    .iter()
                    .copied()
                    .filter(|n| num_to_note.contains_key(n))
                    .collect();
                nums.sort_unstable();
                nums.dedup();
                if nums.is_empty() {
                    continue;
                }
                let line_h = (f64::from(NOTE_CONTENT_PX) * 1.4).ceil();
                let area_h = num::f64_of_usize(nums.len()) * line_h;
                // Bottom of the text band on this page (above the footer margin).
                let band_bottom =
                    fg.page_top + num::f64_of_usize(*page) * fg.page_pitch + fg.letter_h
                        - fg.m.bottom
                        - sy;
                let sep_y = band_bottom - area_h - NOTE_AREA_GAP;
                let left = fg.page_x + fg.m.left;
                let right = fg.page_x + fg.page_w - fg.m.right;
                // Thin separator (about a third of the text width, like Word).
                let sep =
                    vello::kurbo::Line::new((left, sep_y), (left + (right - left) * 0.33, sep_y));
                scene.stroke(
                    &vello::kurbo::Stroke::new(0.75),
                    fg.ctf,
                    NOTE_SEP,
                    None,
                    &sep,
                );
                let mut ny = sep_y + NOTE_AREA_GAP * 0.6;
                for num in nums {
                    if let Some(text) = num_to_note.get(&num) {
                        let line = format!("{num}. {text}");
                        text_renderer.draw_style(
                            scene,
                            &line,
                            NOTE_INK,
                            fg.ctf * Affine::translate((left, ny)),
                            None,
                            &ui::text_render::TextStyle::content(NOTE_CONTENT_PX),
                        );
                        ny += line_h;
                    }
                }
            }
        }
    }

    /// The single-line "start typing" placeholder shown over an otherwise
    /// empty document.
    pub(super) fn draw_empty_placeholder(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        text_renderer: &mut TextRenderer,
        first: Option<&Laid>,
    ) {
        if self.blocks.len() == 1
            && let Some(only) = self.blocks.first()
            && only.kind == "paragraph"
            && only.text.is_empty()
            && let Some(l) = first
        {
            // The hint starts past the caret, so the caret never stands on
            // its first letter, and is set in the paragraph's own size.
            let placeholder = t("canvas-empty-placeholder");
            let style = ui::text_render::TextStyle::content(l.size);
            let x = l.text_x + CARET_WIDTH + f64::from(self.paper_theme.tokens.spacing.s1);
            text_renderer.draw_style(
                scene,
                &placeholder,
                self.paper_theme.tokens.colors.text_tertiary,
                ctf * Affine::translate((x, l.y_top)),
                // The empty-state hint shares the authored paragraph's live
                // column width. At 200% text a one-line hint used to extend
                // beyond the responsive sheet, unlike real paragraph text.
                // Let the shared text renderer wrap it at that exact bound.
                Some(num::f32_of_f64((l.width - (x - l.text_x)).max(1.0))),
                &style,
            );
        }
    }

    /// Header/footer edit mode: veil the body (Word dims it while editing a
    /// running head) on every page, then draw the band caret on top so it
    /// stays crisp through the veil.
    pub(super) fn draw_running_region_veil(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        num_pages: usize,
        sy: f64,
    ) {
        if self.inner.in_running_region() {
            let visible_top = sy + fg.canvas.y0;
            let visible_bottom = sy + fg.canvas.y1 / self.zoom.factor();
            let first_page = num::usize_of_f64(
                ((visible_top - fg.page_top) / fg.page_pitch)
                    .floor()
                    .max(0.0),
            )
            .min(num_pages.saturating_sub(1));
            let last_page = num::usize_of_f64(
                ((visible_bottom - fg.page_top) / fg.page_pitch)
                    .ceil()
                    .max(0.0),
            )
            .saturating_add(1)
            .min(num_pages);
            for i in first_page..last_page {
                let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
                let body_top = ptop + fg.m.top;
                let body_bot = ptop + fg.letter_h - fg.m.bottom;
                let veil = Rect::new(
                    fg.page_x + fg.m.left,
                    body_top,
                    fg.page_x + fg.page_w - fg.m.right,
                    body_bot,
                );
                scene.fill(
                    Fill::NonZero,
                    fg.ctf,
                    with_alpha(self.chrome.paper, BODY_DIM_VEIL_ALPHA),
                    None,
                    &veil,
                );
            }
            self.draw_band_caret(scene, fg.ctf);
        }
    }

    /// How far the document scrolls, in unzoomed content space: the
    /// offset, the overflow and the visible window share one unit, so page
    /// zoom cannot distort the thumb.
    pub(super) fn canvas_scroll_metrics(&self) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            self.max_scroll + self.scroll_viewport,
            self.scroll_viewport,
            self.scroll_y,
        )
    }

    /// The scrollbar thumb (right edge), shown only when the content
    /// overflows the canvas.
    pub(super) fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, canvas: Rect) {
        if self.max_scroll > 0.0 {
            let gutter = f64::from(self.desk_theme.tokens.metrics.scrollbar_gutter);
            let track = Rect::new(canvas.x1 - gutter, canvas.y0, canvas.x1, canvas.y1);
            ui::widget::Scrollbar::vertical().draw(
                scene,
                transform,
                track,
                self.canvas_scroll_metrics(),
                &self.desk_theme,
                false,
            );
        }
    }

    /// Remote collaborators' carets + selections, resolved by the single
    /// [`Self::remote_presence_overlays`] pass so on-screen geometry matches
    /// what the unit tests assert on. Selections paint under the text, carets
    /// over it, each in the peer's colour with a name pill. A no-op with no
    /// remote presence, so single-user rendering is unchanged.
    pub(super) fn draw_remote_overlays(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        canvas_clip: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let (remote_carets, remote_selections) = self.remote_presence_overlays(presence);
        // This pass runs after the page layer was popped. Clip every
        // collaborator's mark and label to the canvas, so a scrolled-away
        // caret or highlight cannot reach the toolbar or other panes.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &canvas_clip,
        );
        // A peer's just-typed run glows in their colour, fading; drawn first so it
        // sits under the selection and caret rather than competing with them.
        for (user, spans) in self.remote_typing_overlays(presence, ui::time::now()) {
            presence.draw_peer_typing(scene, theme, &user, &spans);
        }
        for sel in &remote_selections {
            for r in &sel.rects {
                presence.draw_peer_selection(scene, theme, ctf, &sel.user, *r);
            }
        }
        // Action-driven collaborators publish stable block/range anchors,
        // not fake mouse coordinates. Resolve those anchors through the same
        // shaped layouts as a person's selection and decorate the resulting
        // geometry with the authenticated actor label.
        let now = ui::time::now();
        for interaction in presence.interactions_for_resource("word-processor.document", now) {
            let ranges = match &interaction.anchor {
                tilo_actions::InteractionAnchor::Objects { ids } => ids
                    .iter()
                    .filter_map(|id| {
                        self.blocks
                            .iter()
                            .find(|block| block.id == *id)
                            .map(|block| (id.as_str(), 0, block.text.chars().count()))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut rects = self
                .semantic_text_rects(&interaction.anchor, ctf, canvas_clip)
                .unwrap_or_default();
            for (block, start, end) in ranges {
                let Some(l) = self.laid.iter().find(|laid| laid.id == block) else {
                    continue;
                };
                let Some((_, layout)) = self.layout_cache.get(block) else {
                    continue;
                };
                let (start, end) = (l.layout_byte(start), l.layout_byte(end));
                rects.extend(layout.selection_rects(start, end).into_iter().map(
                    |(x0, y0, x1, y1)| {
                        Rect::new(
                            l.text_x + f64::from(x0),
                            l.y_top + f64::from(y0),
                            l.text_x + f64::from(x1),
                            l.y_top + f64::from(y1),
                        )
                    },
                ));
            }
            if rects.is_empty() {
                continue;
            }
            let user = UserId(interaction.actor.id.clone());
            for rect in &rects {
                presence.draw_peer_selection(scene, theme, ctf, &user, *rect);
            }
            // Label an actual visible span, never a union spanning page gaps.
            if let Some(bounds) = rects.first().copied() {
                let label = presence.display_name(&user);
                presence.draw_peer_outline_labeled(
                    &mut ui::paint::PaintCtx::new(scene, ctf, text_renderer, theme),
                    &user,
                    &label,
                    bounds,
                    3.0,
                );
            }
        }
        // A name is window chrome, not document text: page zoom moves the
        // caret but does not magnify its label.
        let page_to_window = self.zoom.transform(Affine::IDENTITY);
        let window = ctf * page_to_window.inverse();
        for c in &remote_carets {
            presence.draw_peer_caret_labeled(
                scene,
                theme,
                text_renderer,
                window,
                &c.user,
                &c.label,
                page_to_window.transform_rect_bbox(c.rect),
            );
        }
        scene.pop_layer();
    }
}
