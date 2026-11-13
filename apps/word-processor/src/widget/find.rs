//! Find and replace for the window bar's search, which the editor serves as the
//! [`Findable`] it drives: it finds over the whole document, scrolls to the match and
//! replaces through one action, one undo step. Find (Ctrl+F) and Replace (Ctrl+H) open
//! the bar's search; the editor draws no find field of its own.

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
    ESTIMATED_BLOCK_EXTENT, Editor, LETTER_RATIO, MAX_PAGE_W, PAGE_TOP, UtilityBarsGeometry,
};

impl Findable for Editor {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        let count = self.find.find(query, options);
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

impl Editor {
    pub(super) fn follow_find(&mut self, state: &AutoCommit, moved: bool) {
        if self.find.wants_blocks(moved) {
            let blocks = self.search_blocks(state);
            self.find.set_blocks(blocks);
            if self.find.take_first_reveal() {
                self.select_current_match();
            }
        }
    }

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

    pub(super) fn find_now(&mut self, state: &AutoCommit, query: &str) {
        self.find.find(query, FindOptions::default());
        self.follow_find(state, true);
        if self.find.reveal(0) {
            self.select_current_match();
        }
        self.lifecycle.dirty = true;
    }

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

    pub(super) fn scroll_floor(&self) -> f64 {
        -self.find_inset
    }

    /// Navigate a resource-addressed body range after checking it still exists in the
    /// document: a result may be invoked after rendering reordered its local cache.
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
            let content_y = l.y_top + self.scroll_y;
            self.scroll_y = (content_y - 180.0).clamp(self.scroll_floor(), self.max_scroll);
        } else if let Some(index) = self.body_order.position(bid) {
            // An offscreen target is absent from the render cache: its document position
            // gives a first estimate, which the next sync refines.
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

    /// Document search is separate from rendering's bounded cache: built for one Find
    /// and dropped, so every block is seen without pinning offscreen views.
    pub(super) fn search_blocks(&self, state: &AutoCommit) -> Vec<BlockView> {
        self.inner
            .containers
            .as_ref()
            .map_or_else(Vec::new, |c| tilo_doc::model::materialize(state, c).blocks)
    }
}

impl Editor {
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

    fn search(wp: &mut Editor, state: &ui::State, query: &str) -> usize {
        Findable::find(wp, query, FindOptions::default());
        wp.sync(&state.doc, &state.root);
        Findable::find(wp, query, FindOptions::default()).len()
    }

    fn spans(wp: &Editor) -> Vec<(usize, usize)> {
        wp.find.matches().iter().map(|(_, s, e)| (*s, *e)).collect()
    }

    #[test]
    fn find_ranges_follow_edits_before_replacement() {
        let mut state = crate::initialized_state();
        let mut wp = Editor::detached();
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
        let mut wp = Editor::detached();
        wp.sync(&state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "İ cat cat!");
        wp.sync(&state.doc, &state.root);
        assert_eq!(search(&mut wp, &state, "cat"), 2);
        assert_eq!(spans(&wp), vec![(2, 5), (6, 9)]);
        assert_eq!(Findable::replace_all(&mut wp, "dog"), 2);
        wp.apply_find_replacements(&mut ui::widget::Edits::lend(&mut state.doc));
        assert_eq!(wp.search_blocks(&state.doc)[0].text, "İ dog dog!");

        assert_eq!(search(&mut wp, &state, "dog!"), 1);
    }

    fn written(app: &mut TestApp) -> String {
        app.with_surface::<crate::WordProcessorWidget, _>(crate::widget::tests::on_editor(|wp| {
            let lines: Vec<&str> = wp.blocks.iter().map(|b| b.text.as_str()).collect();
            lines.join("\n").trim().to_owned()
        }))
        .unwrap_or_default()
    }

    type TestApp = ui::framework::testing::TestApp<crate::framework::WordProcessorApp>;

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
        app.with_surface(|wp: &mut crate::WordProcessorWidget| {
            assert_eq!(InPlace::of(wp), InPlace::Document);
            driver.open(InPlace::Document);
            driver.drive(wp, "one", options, None, false);
            let abilities = driver.abilities();
            assert!(abilities.replace && abilities.regex);
        })
        .unwrap();
        app.settle();
        app.with_surface(|wp: &mut crate::WordProcessorWidget| {
            driver.drive(wp, "one", options, None, false);
            assert_eq!(driver.position(), Some((1, 2)));
            assert_eq!(
                wp.state().find.active_match().map(|(_, s, e)| (*s, *e)),
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

        app.with_surface(|wp: &mut crate::WordProcessorWidget| {
            driver.drive(wp, "one", options, None, false);
            assert!(driver.replace(wp, "one", options, "1", true));
        })
        .unwrap();
        app.settle();
        assert_eq!(written(&mut app), "1 two 1");
        app.with_surface(|wp: &mut crate::WordProcessorWidget| {
            driver.drive(wp, "one", options, None, true);
            assert!(
                !wp.state().find.is_active(),
                "closing the search forgets it"
            );
        })
        .unwrap();
    }

    #[test]
    fn a_viewer_finds_without_replace() {
        let mut app = TestApp::new().unwrap();
        app.set_role(ui::permissions::Role::Viewer);
        app.settle();
        app.with_surface::<crate::WordProcessorWidget, _>(crate::widget::tests::on_editor(|wp| {
            assert!(wp.read_only());
            assert!(!Findable::supports_replace(wp), "a viewer replaces nothing");
            assert!(Findable::supports_regex(wp));
        }))
        .unwrap();
    }
}
