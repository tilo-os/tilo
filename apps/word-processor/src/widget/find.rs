//! Find and replace for the window bar's search: the editor is the
//! [`Findable`] the bar drives. It finds over the whole document, selects
//! and scrolls to the current match, and replaces through its own action
//! (one [`RichTextAction::ReplaceMatches`], one undo step) with the
//! document in hand. Find (Ctrl+F) and Find and Replace (Ctrl+H) open the
//! bar's search; the editor draws no find field of its own.

use crate::actions::{RegionKind, RichTextAction, Span};
use crate::document_find::PendingReplace;
use crate::i18n::t;
use crate::num;
use crate::word_processor::Caret;
use automerge::AutoCommit;
use tilo_doc::snapshot::BlockView;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FindMatch, FindOptions, Findable};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::pagination::flow_to_page_stack;
use super::{
    ESTIMATED_BLOCK_EXTENT, LETTER_RATIO, MAX_PAGE_W, PAGE_TOP, UtilityBarsGeometry,
    WordProcessorWidget,
};

impl Findable for WordProcessorWidget {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let count = self.find.find(query, options);
        // The paragraphs are read on the next sync, with the document in
        // hand; the bar finds again then (the revision moves).
        self.lifecycle.dirty = true;
        (0..count)
            .map(|index| FindMatch::new(index, index.saturating_add(1)))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        if self.find.reveal(index) {
            self.select_current_match();
        }
        self.lifecycle.dirty = true;
    }

    fn set_find_cover(&mut self, cover: Option<Rect>) {
        self.find_cover = cover;
        self.follow_find_cover();
        self.lifecycle.dirty = true;
    }

    fn clear_find(&mut self) {
        self.find.clear();
        self.lifecycle.dirty = true;
    }

    fn supports_replace(&self) -> bool {
        !self.read_only()
    }

    fn supports_regex(&self) -> bool {
        true
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(found) = self.find.matches().get(index).cloned() else {
            return false;
        };
        self.lifecycle.dirty = true;
        self.find.queue_replace(vec![found], with) > 0
    }

    fn replace_all(&mut self, with: &str) -> usize {
        let all = self.find.matches().to_vec();
        self.lifecycle.dirty = true;
        self.find.queue_replace(all, with)
    }

    fn content_revision(&self) -> u64 {
        self.find.revision()
    }
}

impl WordProcessorWidget {
    /// Read every paragraph again for an open search, when it has none yet
    /// or the document `moved`.
    pub(super) fn follow_find(&mut self, state: &AutoCommit, moved: bool) {
        if self.find.wants_blocks(moved) {
            let blocks = self.search_blocks(state);
            self.find.set_blocks(blocks);
            if self.find.take_first_reveal() {
                self.select_current_match();
            }
        }
    }

    /// Apply the replacements the bar asked for, each as one action of the
    /// editor's: an undo takes one back whole.
    pub(super) fn apply_find_replacements(&mut self, state: &mut ui::widget::Edits<'_>) {
        let pending = self.find.take_pending();
        if pending.is_empty() {
            return;
        }
        for PendingReplace { matches, with } in pending {
            let spans = matches
                .into_iter()
                .map(|(block, start, end)| Span { block, start, end })
                .collect();
            self.perform_action(
                state,
                &RichTextAction::ReplaceMatches {
                    region: RegionKind::Body,
                    spans,
                    ins: with,
                },
            );
        }
        self.refresh_blocks(state.doc());
        self.lifecycle.dirty = true;
    }

    /// Show the document's matches of `query` as the window's find does,
    /// with the first selected: an agent's Find, and a capture's.
    pub(super) fn find_now(&mut self, state: &AutoCommit, query: &str) {
        self.find.find(query, FindOptions::default());
        self.follow_find(state, true);
        if self.find.reveal(0) {
            self.select_current_match();
        }
        self.lifecycle.dirty = true;
    }

    /// Select (so it highlights) + scroll to the active match.
    fn select_current_match(&mut self) {
        if let Some((bid, s, e)) = self.find.active_match().cloned() {
            self.inner.clear_anchor();
            self.inner.set_caret(Caret {
                block: bid.clone(),
                offset: s,
            });
            self.inner.set_anchor();
            self.inner.set_caret(Caret {
                block: bid.clone(),
                offset: e,
            });
            self.scroll_to_match(&bid, s);
        }
    }

    /// Scroll so the line holding char `offset` of `bid` stands a third of
    /// the way down what the canvas shows clear of the window's search (all
    /// of the canvas when the search covers none of it).
    fn scroll_to_match(&mut self, bid: &str, offset: usize) {
        let Some(top) = self.line_top(bid, offset) else {
            self.scroll_to_block(bid);
            return;
        };
        let canvas_top = self
            .zoom
            .screen_to_content(Point::new(self.zoom_viewport.x0, self.zoom_viewport.y0))
            .y;
        let clear = (self.scroll_viewport - self.find_inset).max(0.0);
        let stand = canvas_top + self.find_inset + clear / 3.0;
        self.scroll_y = (top + self.scroll_y - stand).clamp(self.scroll_floor(), self.max_scroll);
    }

    /// The on-screen top (content px, post-scroll) of the laid-out line
    /// holding char `offset` of `bid`.
    fn line_top(&self, bid: &str, offset: usize) -> Option<f64> {
        let block = self.blocks.iter().find(|block| block.id == bid)?;
        let (_, layout) = self.layout_cache.get(bid)?;
        let byte = crate::text_offsets::char_to_byte(&block.text, offset);
        // A block split across pages is laid out once per page: the line
        // is in the slice whose band holds it.
        self.laid
            .iter()
            .filter(|laid| laid.id == bid && laid.text == block.text)
            .find_map(|laid| {
                let (_, y, height) = layout.caret_geometry_bounded(laid.shown(byte))?;
                let top = laid.layout_y + f64::from(y);
                (top + f64::from(height) > laid.y_top && top < laid.y_top + laid.height)
                    .then_some(top)
            })
    }

    /// Let the document scroll before its start by as much of the canvas as
    /// the window's search covers; what is shown stays where it is until
    /// the cover goes, when the document comes back to its start.
    pub(super) fn follow_find_cover(&mut self) {
        let canvas = self.zoom_viewport;
        let zoom = self.zoom.factor();
        let inset = self
            .find_cover
            .filter(|cover| cover.x0 < canvas.x1 && cover.x1 > canvas.x0 && cover.y0 < canvas.y1)
            .map_or(0.0, |cover| {
                ((cover.y1 - canvas.y0) / zoom).clamp(0.0, canvas.height() / zoom)
            });
        if inset.to_bits() != self.find_inset.to_bits() {
            self.find_inset = inset;
            self.scroll_y = self.scroll_y.clamp(self.scroll_floor(), self.max_scroll);
        }
    }

    /// The least the document scrolls to: before its start while the
    /// window's search covers the canvas.
    pub(super) fn scroll_floor(&self) -> f64 {
        -self.find_inset
    }

    /// Navigate a resource-addressed body range after checking it still exists
    /// in the canonical document. This is intentionally separate from Find's
    /// index because global search/agents may invoke a result after rendering
    /// has virtualized or reordered its local cache.
    pub(super) fn navigate_to_range(
        &mut self,
        state: &AutoCommit,
        block: &str,
        start: usize,
        end: usize,
    ) -> Result<(), String> {
        let Some(containers) = self.inner.containers.as_ref() else {
            return Err("Writer document is not initialized".into());
        };
        let Some(text) = tilo_doc::model::block_text(state, containers, block) else {
            return Err("the requested document range no longer exists".into());
        };
        let len = text.chars().count();
        if start > end || end > len {
            return Err("the requested document range is stale".into());
        }
        if self.inner.in_running_region() {
            return Err("leave header or footer editing before navigating body results".into());
        }
        self.inner.clear_anchor();
        self.inner.set_caret(Caret {
            block: block.to_string(),
            offset: start,
        });
        if start < end {
            self.inner.set_anchor();
            self.inner.set_caret(Caret {
                block: block.to_string(),
                offset: end,
            });
        }
        self.scroll_to_block(block);
        self.lifecycle.dirty = true;
        Ok(())
    }

    pub(super) fn scroll_to_block(&mut self, bid: &str) {
        if let Some(l) = self.laid.iter().find(|l| l.id == bid) {
            // l.y_top is screen-space (post-scroll); recover content space.
            let content_y = l.y_top + self.scroll_y;
            self.scroll_y = (content_y - 180.0).clamp(self.scroll_floor(), self.max_scroll);
        } else if let Some(index) = self.body_order.position(bid) {
            // Find can target an offscreen block that is intentionally absent
            // from the bounded render cache. Its stable canonical position gives
            // us a cheap initial scroll estimate; the next sync loads/shapes the
            // local window and refines the visual position.
            let content_y = self.zoom.anchor().y
                + PAGE_TOP
                + flow_to_page_stack(
                    num::f64_of_usize(index) * ESTIMATED_BLOCK_EXTENT,
                    MAX_PAGE_W * LETTER_RATIO,
                    self.margins,
                );
            self.scroll_y = (content_y - 180.0).clamp(self.scroll_floor(), self.max_scroll);
        }
    }

    /// Complete document search is intentionally separate from rendering's
    /// bounded cache. It is built transiently for a Find operation, then dropped,
    /// so search/resource identity sees every block without pinning offscreen
    /// rich views in the frame cache.
    pub(super) fn search_blocks(&self, state: &AutoCommit) -> Vec<BlockView> {
        self.inner
            .containers
            .as_ref()
            .map_or_else(Vec::new, |c| tilo_doc::model::materialize(state, c).blocks)
    }
}

impl WordProcessorWidget {
    /// Draw any failed-save banner in the document flow. Return the height
    /// reserved below the toolbar.
    pub(super) fn draw_save_error(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        geometry: &UtilityBarsGeometry,
    ) -> f64 {
        let UtilityBarsGeometry {
            left: canvas_left,
            right: canvas_right,
            top: tb_bottom,
        } = *geometry;
        // A failed save stays in view, in the flow above the page, until it
        // is resolved: an error banner, not a toast over the text.
        if self.save_error.is_none() {
            return 0.0;
        }
        let sp = &theme.tokens.spacing;
        let (across, along) = (f64::from(sp.s3), f64::from(sp.s2));
        let title = t("toast-save-failed");
        let content =
            ui::widget::BannerContent::new(ui::widget::BannerKind::Error, &title).dismissible(true);
        let width = (canvas_right - canvas_left - 2.0 * across).max(1.0);
        let h = ui::widget::Banner::height(&content, width, text_renderer, theme);
        let top = tb_bottom + along;
        let rect = Rect::new(canvas_left + across, top, canvas_right - across, top + h);
        let hit = ui::widget::Banner::paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            &content,
            self.pointer,
        );
        self.save_error = Some(hit.dismiss.unwrap_or(Rect::ZERO));
        h + 2.0 * along
    }
}

#[cfg(test)]
mod regressions {
    use super::*;
    use ui::widget::Widget;

    /// The bar's search on `wp`: the query, then the paragraphs read with
    /// the document on the next sync, then the query again as the bar does
    /// when the revision moves.
    fn search(wp: &mut WordProcessorWidget, state: &ui::State, query: &str) -> usize {
        Findable::find(wp, query, FindOptions::default());
        wp.sync(&state.doc, &state.root);
        Findable::find(wp, query, FindOptions::default()).len()
    }

    fn spans(wp: &WordProcessorWidget) -> Vec<(usize, usize)> {
        wp.find.matches().iter().map(|(_, s, e)| (*s, *e)).collect()
    }

    #[test]
    fn find_ranges_follow_edits_before_replacement() {
        let mut state = crate::initialized_state();
        let mut wp = WordProcessorWidget::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "cat tail");
        wp.sync(&state.doc, &state.root);
        assert_eq!(search(&mut wp, &state, "cat"), 1);
        let block = wp.inner.caret.block.clone();
        wp.inner.set_caret(Caret { block, offset: 0 });
        wp.model_mut()
            .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "new ");
        let caret = wp.inner.caret.clone();

        // The normal sync follows document changes, without moving the
        // person's caret back to a match.
        wp.sync(&state.doc, &state.root);
        assert_eq!(wp.inner.caret, caret);
        assert_eq!(spans(&wp), vec![(4, 7)]);
        assert_eq!(Findable::replace_all(&mut wp, "dog"), 1);
        assert!(wp.find.matches().is_empty(), "the count follows at once");
        wp.apply_find_replacements(&mut ui::widget::Edits::lend(&mut state.doc));
        assert_eq!(wp.search_blocks(&state.doc)[0].text, "new dog tail");
        assert!(wp.find.matches().is_empty());
    }

    #[test]
    fn literal_replace_preserves_text_after_expanding_unicode_case() {
        let mut state = crate::initialized_state();
        let mut wp = WordProcessorWidget::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "İ cat cat!");
        wp.sync(&state.doc, &state.root);
        assert_eq!(search(&mut wp, &state, "cat"), 2);
        assert_eq!(spans(&wp), vec![(2, 5), (6, 9)]);
        assert_eq!(Findable::replace_all(&mut wp, "dog"), 2);
        wp.apply_find_replacements(&mut ui::widget::Edits::lend(&mut state.doc));
        assert_eq!(wp.search_blocks(&state.doc)[0].text, "İ dog dog!");

        // Literal search must not treat punctuation as a regular expression.
        assert_eq!(search(&mut wp, &state, "dog!"), 1);
    }

    /// The document's text, its paragraphs one per line.
    fn written(app: &mut TestApp) -> String {
        app.with_surface::<WordProcessorWidget, _>(|wp| {
            let lines: Vec<&str> = wp.blocks.iter().map(|b| b.text.as_str()).collect();
            lines.join("\n").trim().to_owned()
        })
        .unwrap_or_default()
    }

    type TestApp = ui::framework::testing::TestApp<crate::framework::WordProcessorApp>;

    /// The window bar's search finds in the document, offers match case,
    /// whole word, regular expressions and replace, and a replacement is one
    /// step of the document's own undo.
    #[test]
    fn the_window_bars_search_finds_and_replaces_in_the_document() {
        use ui::widget::{InPlace, SearchDriver};
        let mut app = TestApp::new().unwrap();
        app.invoke(crate::model::Document::APPEND_PARAGRAPH.with("one two one"))
            .unwrap();
        app.settle();
        assert_eq!(written(&mut app), "one two one");
        let options = FindOptions::default();
        let mut driver = SearchDriver::default();
        app.with_surface::<WordProcessorWidget, _>(|wp| {
            assert_eq!(InPlace::of(wp), InPlace::Document);
            driver.open(InPlace::Document);
            driver.drive(wp, "one", options, None, false);
            let abilities = driver.abilities();
            assert!(abilities.replace && abilities.regex);
        })
        .unwrap();
        app.settle();
        app.with_surface::<WordProcessorWidget, _>(|wp| {
            driver.drive(wp, "one", options, None, false);
            assert_eq!(driver.position(), Some((1, 2)));
            assert_eq!(
                wp.find.active_match().map(|(_, s, e)| (*s, *e)),
                Some((0, 3)),
                "the first match is selected"
            );
            driver.drive(wp, "one", options, Some(true), false);
            assert!(driver.replace(wp, "one", options, "three", false));
        })
        .unwrap();
        app.settle();
        assert_eq!(written(&mut app), "one two three");
        assert!(app.undo(), "the replacement is one undo step");
        app.settle();
        assert_eq!(written(&mut app), "one two one");

        app.with_surface::<WordProcessorWidget, _>(|wp| {
            driver.drive(wp, "one", options, None, false);
            assert!(driver.replace(wp, "one", options, "1", true));
        })
        .unwrap();
        app.settle();
        assert_eq!(written(&mut app), "1 two 1");
        app.with_surface::<WordProcessorWidget, _>(|wp| {
            driver.drive(wp, "one", options, None, true);
            assert!(!wp.find.is_active(), "closing the search forgets it");
        })
        .unwrap();
    }

    #[test]
    fn a_viewer_finds_without_replace() {
        let mut app = TestApp::new().unwrap();
        app.set_role(ui::permissions::Role::Viewer);
        app.settle();
        app.with_surface::<WordProcessorWidget, _>(|wp| {
            assert!(wp.read_only());
            assert!(!Findable::supports_replace(wp), "a viewer replaces nothing");
            assert!(Findable::supports_regex(wp));
        })
        .unwrap();
    }
}
