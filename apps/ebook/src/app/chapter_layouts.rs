//! The chapter layouts the reader holds: the one on screen, the next one it
//! will show while it arrives, and the chapters either side of it, set
//! ahead by the layout worker so turning or scrolling into them never waits.

use std::sync::Arc;

use ui::paint::PaintCtx;
use ui::text_render::{FontChoice, TextRenderer};
use ui::widget::{SkeletonFace, SkeletonShape};
use vello::kurbo::Rect;

use super::{BooksApp, Pane, ReaderPageGeometry};
use crate::layout_worker::{LayoutJob, LayoutWorker};
use crate::model::{Book, ReadMode};
use crate::paginate::{
    ChapterLayout, ChapterLayoutRequest, ChapterLayouter, LayoutKey, layout_chapter,
};
use crate::reader_theme::ReaderPalette;

/// A layout and the request it answers: stretches the worker hands over
/// for that request extend it.
struct Held {
    ticket: u64,
    layout: ChapterLayout,
}

pub(super) struct ChapterLayouts {
    worker: Option<LayoutWorker>,
    /// The worker could not start (no threads on the web): the reader lays
    /// out in its frame.
    inline: bool,
    /// The book the held layouts are of.
    book: String,
    held: Vec<Held>,
    /// The request the layout on screen answers.
    shown: u64,
    /// The layout the reader last wanted on screen.
    wanted: Option<LayoutKey>,
    /// The layout on screen is the part of this one the reader set itself
    /// to show the reader's place at once: it stands in until the worker's
    /// layout of the same chapter reaches that place.
    standin: Option<LayoutKey>,
    next_ticket: u64,
}

impl Default for ChapterLayouts {
    fn default() -> Self {
        Self {
            worker: None,
            inline: false,
            book: String::new(),
            held: Vec::new(),
            shown: 0,
            wanted: None,
            standin: None,
            next_ticket: 1,
        }
    }
}

impl ChapterLayouts {
    fn held(&self, key: LayoutKey) -> Option<&ChapterLayout> {
        self.held
            .iter()
            .find(|held| held.layout.key == key)
            .map(|held| &held.layout)
    }

    fn take(&mut self, key: LayoutKey) -> Option<Held> {
        let at = self.held.iter().position(|held| held.layout.key == key)?;
        Some(self.held.swap_remove(at))
    }

    fn ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }
}

/// A layout with nothing set yet, for `key`.
fn unset(key: LayoutKey) -> ChapterLayout {
    ChapterLayout {
        blocks: Vec::new(),
        total_height: 0.0,
        pages: Vec::new(),
        key,
        complete: false,
    }
}

/// The key of `chapter` laid out as `key` is.
fn of_chapter(key: LayoutKey, chapter: usize) -> LayoutKey {
    LayoutKey { chapter, ..key }
}

/// What the reader lays a chapter out with this frame.
pub(super) struct LayoutInputs<'a> {
    pub book: &'a Arc<Book>,
    pub geom: &'a ReaderPageGeometry,
    pub font_sig: u64,
    pub base_family: &'a FontChoice,
    pub font_mult: f32,
}

impl BooksApp {
    /// Put the layout on screen aside for a different chapter: it stays held
    /// while it neighbours the chapter shown next.
    pub(super) fn retire_layout(&mut self) {
        if let Some(layout) = self.layout.take() {
            let ticket = self.layouts.shown;
            self.layouts.held.push(Held { ticket, layout });
        }
    }

    /// Take in what the worker handed over. Whether any of it shows: the
    /// layout on screen grew, or the one to show next did.
    pub(super) fn absorb_layout_chunks(&mut self) -> bool {
        let Some(worker) = self.layouts.worker.as_ref() else {
            return false;
        };
        if !worker.has_arrivals() {
            return false;
        }
        let mut shows = false;
        for chunk in worker.take() {
            if chunk.book != self.layouts.book {
                continue;
            }
            let layouts = &mut self.layouts;
            let answers_shown = layouts.shown == chunk.ticket;
            let target = match self.layout.as_mut() {
                Some(layout) if answers_shown && layout.key == chunk.key => {
                    shows = true;
                    layout
                }
                _ => match layouts
                    .held
                    .iter_mut()
                    .find(|held| held.layout.key == chunk.key && held.ticket == chunk.ticket)
                {
                    Some(held) => {
                        shows |= held.layout.key.chapter == self.chapter;
                        &mut held.layout
                    }
                    None => continue,
                },
            };
            target.blocks.extend(chunk.blocks);
            target.pages.extend(chunk.pages);
            target.total_height = chunk.height;
            target.complete = chunk.complete;
        }
        shows
    }

    /// The layout to draw this frame for the current chapter at `geom`. The
    /// worker sets it; until enough of it is set to show where the reader
    /// is, the chapter's previous layout (a resize, a new text size) or a
    /// layout with nothing set stands in. Asks the worker for whatever the
    /// chapter and its neighbours still lack.
    pub(super) fn resolve_chapter_layout(
        &mut self,
        tr: &mut TextRenderer,
        inputs: &LayoutInputs<'_>,
    ) -> ChapterLayout {
        let geom = inputs.geom;
        let key = LayoutKey::new(
            self.chapter,
            geom.content_w,
            geom.content_h,
            &self.settings,
            inputs.font_sig,
            tr.text_scale(),
        );
        let book = inputs.book;
        self.layouts.wanted = Some(key);
        if self.layouts.book != book.id {
            self.layouts.book.clone_from(&book.id);
            self.layouts.held.clear();
            self.layout = None;
        }
        if self.layouts.worker.is_none() && !self.layouts.inline {
            self.layouts.worker = LayoutWorker::spawn();
            self.layouts.inline = self.layouts.worker.is_none();
        }
        self.absorb_layout_chunks();
        let previous = self.layout.as_ref().map(|l| l.key);
        let standing_in = self.layouts.standin == Some(key) && previous == Some(key);
        if !standing_in {
            self.layouts.standin = None;
        }
        if previous != Some(key) || standing_in {
            // The same chapter laid out again (a resize, a new text size or
            // font): keep the paragraph the reader was at on screen, rather
            // than a page number that now holds other text.
            if !standing_in
                && previous.is_some_and(|old| old.chapter == self.chapter)
                && self.pending_jump_block.is_none()
                && self.pending_seek_fraction.is_none()
                && !self.pending_last_page
                && !self.settings.mode.is_continuous()
            {
                self.pending_jump_block = Some(self.page_top_block);
            }
            if self.layouts.inline {
                self.layout = Some(self.layout_inline(tr, inputs, key));
            } else if let Some(next) = self.layouts.take(key) {
                if self.layout_settles(&next.layout, geom.content_h) {
                    if previous.is_some_and(|old| old.chapter != self.chapter) {
                        self.retire_layout();
                    }
                    // The part set to stand in goes: the worker's layout
                    // has the same text from the chapter's start.
                    self.layouts.standin = None;
                    self.layouts.shown = next.ticket;
                    self.layout = Some(next.layout);
                } else {
                    self.layouts.held.push(next);
                }
            }
        }
        if !self.layouts.inline {
            self.ask_for_layouts(tr, inputs, key);
        }
        match self.layout.take() {
            Some(layout) if layout.key.chapter == self.chapter => layout,
            other => {
                self.layout = other;
                self.retire_layout();
                self.lay_out_place(tr, inputs, key)
            }
        }
    }

    /// Set as much of the chapter as the reader's place needs, here and
    /// now, from its start: a chapter is text on this device, so the frame
    /// shows its lines however far the worker has got. The worker goes on
    /// setting the whole chapter, and its layout takes over once it reaches
    /// the place ([`Self::layout_settles`]).
    fn lay_out_place(
        &mut self,
        tr: &mut TextRenderer,
        inputs: &LayoutInputs<'_>,
        key: LayoutKey,
    ) -> ChapterLayout {
        let blocks = inputs
            .book
            .chapters
            .get(self.chapter)
            .map_or(&[][..], |chapter| &chapter.blocks);
        let request = ChapterLayoutRequest {
            blocks,
            content_width: inputs.geom.content_w,
            content_height: inputs.geom.content_h,
            settings: &self.settings,
            chapter: key.chapter,
            language: &inputs.book.language,
            base_family: inputs.base_family,
            font_multiplier: inputs.font_mult,
            font_signature: inputs.font_sig,
        };
        let mut layouter = ChapterLayouter::new(&request, tr.text_scale());
        let mut place = unset(key);
        while !self.layout_settles(&place, inputs.geom.content_h) {
            let Some(block) = layouter.next_block(tr) else {
                place.pages.extend(layouter.finish_pages());
                place.total_height = layouter.height();
                place.complete = true;
                return place;
            };
            place.blocks.push(block);
            place.pages.extend(layouter.take_pages());
            place.total_height = layouter.height();
        }
        self.layouts.standin = Some(key);
        place
    }

    fn layout_inline(
        &self,
        tr: &mut TextRenderer,
        inputs: &LayoutInputs<'_>,
        key: LayoutKey,
    ) -> ChapterLayout {
        let blocks = inputs
            .book
            .chapters
            .get(self.chapter)
            .map_or(&[][..], |chapter| &chapter.blocks);
        layout_chapter(
            tr,
            &ChapterLayoutRequest {
                blocks,
                content_width: inputs.geom.content_w,
                content_height: inputs.geom.content_h,
                settings: &self.settings,
                chapter: key.chapter,
                language: &inputs.book.language,
                base_family: inputs.base_family,
                font_multiplier: inputs.font_mult,
                font_signature: inputs.font_sig,
            },
        )
    }

    /// Whether enough of `layout` is set to show the reader's place in it:
    /// the page or screen it is going to, and in a scroll the screen's
    /// worth below it. A place counted from the chapter's end waits for the
    /// whole chapter.
    fn layout_settles(&self, layout: &ChapterLayout, view_h: f32) -> bool {
        if layout.complete {
            return true;
        }
        if self.pending_last_page || self.pending_seek_fraction.is_some() || self.pending_scroll_end
        {
            return false;
        }
        let top = match self.pending_jump_block {
            Some(block) => match layout.blocks.iter().find(|b| b.index == block) {
                Some(b) => b.top,
                None => return false,
            },
            None if self.settings.mode.is_continuous() => self.scroll_y as f32,
            None => layout.pages.get(self.page).copied().unwrap_or(f32::MAX),
        };
        if self.settings.mode.is_continuous() {
            return layout.total_height >= top + view_h;
        }
        let page = layout
            .pages
            .iter()
            .rposition(|&start| start <= top + 0.5)
            .unwrap_or(0);
        layout.pages.len() > page + self.settings.mode.page_stride()
    }

    /// Ask the worker for the chapter on screen and its neighbours, in that
    /// order, as far as they are not set; and let go of layouts of any other
    /// chapter or geometry.
    fn ask_for_layouts(&mut self, tr: &TextRenderer, inputs: &LayoutInputs<'_>, key: LayoutKey) {
        let chapters = inputs.book.chapters.len();
        let mut wanted = vec![key];
        if key.chapter + 1 < chapters {
            wanted.push(of_chapter(key, key.chapter + 1));
        }
        if key.chapter > 0 {
            wanted.push(of_chapter(key, key.chapter - 1));
        }
        self.layouts
            .held
            .retain(|held| wanted.contains(&held.layout.key));
        let mut jobs = Vec::new();
        let standin = self.layouts.standin;
        for want in wanted {
            let ticket = match self.layout.as_ref() {
                Some(layout) if layout.key == want && standin != Some(want) => {
                    if layout.complete {
                        continue;
                    }
                    self.layouts.shown
                }
                _ => {
                    if let Some(held) = self.layouts.held.iter().find(|h| h.layout.key == want) {
                        if held.layout.complete {
                            continue;
                        }
                        held.ticket
                    } else {
                        let ticket = self.layouts.ticket();
                        self.layouts.held.push(Held {
                            ticket,
                            layout: unset(want),
                        });
                        ticket
                    }
                }
            };
            jobs.push(LayoutJob {
                key: want,
                ticket,
                book: Arc::clone(inputs.book),
                settings: self.settings.clone(),
                content_width: inputs.geom.content_w,
                content_height: inputs.geom.content_h,
                text_scale: tr.text_scale(),
                base_family: inputs.base_family.clone(),
                font_multiplier: inputs.font_mult,
            });
        }
        if let Some(worker) = self.layouts.worker.as_mut() {
            worker.want(tr, jobs);
        }
    }

    /// Whether the reader shows a chapter not yet set as far as it needs.
    pub(super) fn layout_pending(&self) -> bool {
        self.layouts.worker.is_some()
            && self
                .layout
                .as_ref()
                .is_none_or(|l| !l.complete || Some(l.key) != self.layouts.wanted)
    }

    /// Whether the scroll may roll on into `chapter`: the reader shows it at
    /// once there. Into the next chapter its first screen must be set; back
    /// into the previous one, all of it, since the scroll lands on its end.
    fn may_roll_into(&self, chapter: usize, forward: bool) -> bool {
        if self.layouts.inline {
            return true;
        }
        let Some(shown) = self
            .layout
            .as_ref()
            .filter(|l| l.key.chapter == self.chapter)
        else {
            return false;
        };
        self.layouts
            .held(of_chapter(shown.key, chapter))
            .is_some_and(|next| {
                next.complete || (forward && next.total_height >= shown.key.height_q as f32)
            })
    }

    /// Move the continuous-scroll offset by `delta` px, rolling into the
    /// neighbouring chapter when the scroll runs off either end, once that
    /// chapter is set; until then the scroll rests at the end it reached.
    pub(super) fn scroll_continuous(&mut self, delta: f64) {
        let (total_h, view_h) = self.flow_extent;
        let max_scroll = (total_h - view_h).max(0.0);
        let target = self.scroll_y + delta;
        let complete = self.layouts.inline || self.layout.as_ref().is_some_and(|l| l.complete);
        if target < -0.5 && self.chapter > 0 && self.may_roll_into(self.chapter - 1, false) {
            self.retire_layout();
            self.chapter -= 1;
            self.selected_block = None;
            self.pending_scroll_end = true;
            self.scroll_y = 0.0;
        } else if target > max_scroll + 0.5
            && complete
            && self.chapter + 1 < self.num_chapters()
            && self.may_roll_into(self.chapter + 1, true)
        {
            self.retire_layout();
            self.chapter += 1;
            self.selected_block = None;
            self.scroll_y = 0.0;
        } else {
            self.scroll_y = target.clamp(0.0, max_scroll);
        }
    }

    pub(super) fn resolve_pending_seek_fraction(
        &mut self,
        layout: &ChapterLayout,
        mode: ReadMode,
        geom: &ReaderPageGeometry,
    ) {
        if !layout.complete {
            return;
        }
        let Some(within) = self.pending_seek_fraction.take() else {
            return;
        };
        if mode.is_continuous() {
            self.scroll_y = (f64::from(layout.total_height) - f64::from(geom.content_h)).max(0.0)
                * f64::from(within);
        } else {
            let count = layout.page_count().max(1);
            let page = ((within * count as f32).floor() as usize).min(count - 1);
            self.page = page - page % mode.page_stride();
        }
    }

    /// Resolve the current page/scroll index against the layout: honour a
    /// pending "jump to last page"/"jump to block" request once the layout
    /// holds its target, clamp to the page count, align a spread to an even
    /// left page, and (continuous mode) clamp the scroll offset and derive
    /// `self.page` from it.
    pub(super) fn resolve_reader_page_index(
        &mut self,
        layout: &ChapterLayout,
        mode: ReadMode,
        geom: &ReaderPageGeometry,
    ) {
        if self.pending_last_page && layout.complete {
            self.page = layout.page_count().saturating_sub(1);
            self.pending_last_page = false;
        }
        if let Some(jb) = self.pending_jump_block
            && (layout.complete || layout.blocks.iter().any(|b| b.index == jb))
        {
            self.pending_jump_block = None;
            self.page = super::page_for_block(layout, jb);
            if mode.is_continuous() {
                self.scroll_y = layout
                    .blocks
                    .iter()
                    .find(|b| b.index == jb)
                    .map_or(0.0, |b| f64::from(b.top));
            }
        }
        self.page = self.page.min(layout.page_count().saturating_sub(1));
        // Spreads align to an even left page so a spread is always (2k, 2k+1).
        if mode == ReadMode::TwoUp {
            self.page -= self.page % 2;
        }

        // Record the flow extent so a continuous scroll (which runs before the
        // next layout) can clamp itself, and resolve a pending end-scroll after
        // a backward chapter roll. Keep `page` synced to the scroll so progress
        // and the page locator stay meaningful.
        self.flow_extent = (f64::from(layout.total_height), f64::from(geom.content_h));
        self.flow_complete = layout.complete;
        if mode.is_continuous() {
            let max_scroll = (f64::from(layout.total_height) - f64::from(geom.content_h)).max(0.0);
            if self.pending_scroll_end && layout.complete {
                self.scroll_y = max_scroll;
                self.pending_scroll_end = false;
            }
            if !layout.blocks.is_empty() {
                self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
            }
            self.page = layout
                .pages
                .iter()
                .rposition(|&py| f64::from(py) <= self.scroll_y + 0.5)
                .unwrap_or(0);
        } else {
            self.pending_scroll_end = false;
        }
    }

    /// Where a pane's text is not set yet, the shape of the lines that will
    /// stand there.
    pub(super) fn draw_unset_text(
        ctx: &mut PaintCtx<'_>,
        layout: &ChapterLayout,
        panes: &[Pane],
        geom: &ReaderPageGeometry,
        (palette, line_pitch): (&ReaderPalette, f64),
    ) {
        if layout.complete {
            return;
        }
        let bottom = geom.content_y + f64::from(geom.content_h);
        for pane in panes {
            // A page whose end is not decided yet runs to the column's foot.
            let page_open = !layout
                .pages
                .iter()
                .any(|&start| start > pane.win_start + 0.5);
            if !pane.partial && !page_open {
                continue;
            }
            let set_to = if layout.blocks.is_empty() {
                pane.win_start
            } else {
                layout.total_height.max(pane.win_start)
            };
            if set_to >= pane.win_start + geom.content_h {
                continue;
            }
            let top = pane.screen_y(set_to).max(geom.content_y);
            let area = Rect::new(pane.x, top, pane.x + f64::from(pane.w), bottom);
            paint_text_skeleton(ctx, area, palette, line_pitch);
        }
    }
}

/// Paragraph shapes down `area` at the page's line pitch, in the skeleton
/// tone of the page, so the lines that arrive land where their shapes were.
fn paint_text_skeleton(ctx: &mut PaintCtx<'_>, area: Rect, palette: &ReaderPalette, pitch: f64) {
    const LINES: usize = 5;
    let mut theme = ctx.theme.clone();
    theme.tokens.colors.surfaces.content = palette.page;
    let mut ctx = PaintCtx {
        theme: &theme,
        ..ctx.reborrow()
    };
    let face = SkeletonFace::new(SkeletonShape::Text { lines: 1 });
    let bar = face.height(ctx.text, &theme, area.width());
    let rows = (area.height() / pitch).floor().max(0.0) as usize;
    for row in 0..rows {
        let in_paragraph = row % (LINES + 1);
        if in_paragraph == LINES {
            continue;
        }
        let width = if in_paragraph + 1 == LINES {
            area.width() * 0.6
        } else {
            area.width()
        };
        let top = area.y0 + row as f64 * pitch + (pitch - bar) / 2.0;
        face.paint(
            &mut ctx,
            Rect::new(area.x0, top, area.x0 + width, top + bar),
        );
    }
}
