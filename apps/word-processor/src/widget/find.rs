//! Find and replace: the find bar's host, moving between matches and
//! scrolling them into view.

use crate::find_bar::{Blocks, FindEffect, FindHost, FindMatch};
use crate::i18n::t;
use crate::num;
use crate::word_processor::Caret;
use automerge::AutoCommit;
use tilo_doc::snapshot::BlockView;
use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::pagination::flow_to_page_stack;
use super::{
    ESTIMATED_BLOCK_EXTENT, EditorFind, LETTER_RATIO, MAX_PAGE_W, PAGE_TOP, UtilityBarsGeometry,
    WordProcessorWidget,
};

impl WordProcessorWidget {
    // ---- Find & Replace -----------------------------------------------------

    pub(super) fn open_find(&mut self, state: &AutoCommit) {
        // Seed the query from a single-line selection, if any.
        let seed = if let Some((bid, s, e)) = self.selection_blocks_cached().first().cloned()
            && let Some(b) = self.blocks.iter().find(|b| b.id == bid)
        {
            let sel: String = b.text.chars().skip(s).take(e.saturating_sub(s)).collect();
            (!sel.is_empty() && !sel.contains('\n')).then_some(sel)
        } else {
            None
        };
        let blocks = self.search_blocks(state);
        let effect = self.find.open(seed.as_deref(), &mut Blocks(&blocks));
        self.apply_find_effect(state, effect);
    }

    pub(super) fn close_find(&mut self) {
        self.find.close();
    }

    /// Select (so it highlights) + scroll to the active match.
    fn select_current_match(&mut self, _state: &AutoCommit) {
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
            self.scroll_to_block(&bid);
        }
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
            self.scroll_y = (content_y - 180.0).clamp(0.0, self.max_scroll);
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
            self.scroll_y = (content_y - 180.0).clamp(0.0, self.max_scroll);
        }
    }

    pub(super) fn apply_find_effect(&mut self, state: &AutoCommit, effect: FindEffect) {
        if effect == FindEffect::SelectActive {
            self.select_current_match(state);
        }
    }

    /// Carry what the person did in the Find bar to the document: search
    /// again over the whole document, step to a match and select it, or
    /// replace through the editor's own actions.
    pub(super) fn drive_find(&mut self, state: &mut ui::widget::Edits<'_>) {
        let mut find = std::mem::take(&mut self.find);
        let effect = find.drive(&mut EditorFind { wp: self, state });
        self.find = find;
        self.apply_find_effect(state, effect);
    }

    #[cfg(test)]
    pub(super) fn replace_all_matches(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        with: &str,
    ) -> usize {
        let matches = self.find.matches().to_vec();
        let replaced = EditorFind { wp: self, state }.replace_matches(&matches, with);
        let mut find = std::mem::take(&mut self.find);
        let effect = find.refresh(&mut EditorFind { wp: self, state });
        self.find = find;
        self.apply_find_effect(state, effect);
        replaced
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
    /// Draw the shared Find bar and any failed-save banner in the document flow.
    /// Return the height reserved below the toolbar.
    pub(super) fn draw_find_bar(
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
        let mut utility_h = 0.0;
        // A failed save stays in view, in the flow above the page, until it
        // is resolved: an error banner, not a toast over the text.
        if self.save_error.is_some() {
            let sp = &theme.tokens.spacing;
            let (across, along) = (f64::from(sp.s3), f64::from(sp.s2));
            let title = t("toast-save-failed");
            let content = ui::widget::BannerContent::new(ui::widget::BannerKind::Error, &title)
                .dismissible(true);
            let width = (canvas_right - canvas_left - 2.0 * across).max(1.0);
            let h = ui::widget::Banner::height(&content, width, text_renderer, theme);
            let top = tb_bottom + utility_h + along;
            let rect = Rect::new(canvas_left + across, top, canvas_right - across, top + h);
            let hit = ui::widget::Banner::paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                &content,
                self.pointer,
            );
            self.save_error = Some(hit.dismiss.unwrap_or(Rect::ZERO));
            utility_h += h + 2.0 * along;
        }
        let find_h = self.find.height(theme);
        self.find_band = Rect::new(
            canvas_left,
            tb_bottom + utility_h,
            canvas_right,
            tb_bottom + utility_h + find_h,
        );
        if find_h > 0.0 {
            let presence = PresenceCtx::empty();
            presence.viewport.set(Some(self.rect));
            self.find.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &presence,
                self.find_band,
            );
        }
        utility_h + find_h
    }
}

impl FindHost for EditorFind<'_, '_> {
    fn search_blocks(&mut self) -> Vec<BlockView> {
        self.wp.search_blocks(self.state)
    }

    fn replace_matches(&mut self, matches: &[FindMatch], with: &str) -> usize {
        let mut matches = matches.to_vec();
        // Replace from the highest char offset down so earlier offsets in a
        // block stay valid.
        matches.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        for (block, start, end) in matches.iter().rev() {
            self.wp.splice_via_action(
                self.state,
                crate::actions::RegionKind::Body,
                block,
                *start,
                *end,
                with,
            );
        }
        if !matches.is_empty() {
            self.wp.refresh_blocks(self.state);
            self.wp.lifecycle.dirty = true;
        }
        matches.len()
    }
}
