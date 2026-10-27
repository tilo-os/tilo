//! Scrolling the client's lists: the region under a pointer, the offsets it keeps, the edge fades and bars it paints, and a press or drag on a bar.

use super::*;

impl GitClientApp {
    pub(super) fn scroll_at(&mut self, pos: Point, dx: f64, dy: f64) {
        let Some(region) = self
            .scroll_regions
            .iter()
            .rev()
            .find(|r| r.rect.contains(pos))
            .copied()
        else {
            return;
        };
        let max_y = region.max_y.max(0.0);
        let max_x = region.max_x.max(0.0);
        self.bar_fade.wake(ui::time::now());
        match region.scroller {
            Scroller::ChangesList => {
                self.changes_scroll = (self.changes_scroll - dy).clamp(0.0, max_y);
            }
            Scroller::Diff => {
                self.diff_scroll = (self.diff_scroll - dy).clamp(0.0, max_y);
                self.diff_hscroll = (self.diff_hscroll - dx).clamp(0.0, max_x);
            }
            Scroller::HistoryList => {
                self.history_scroll = (self.history_scroll - dy).clamp(0.0, max_y);
            }
            Scroller::Detail => self.detail_scroll = (self.detail_scroll - dy).clamp(0.0, max_y),
            Scroller::Panels => self.panels_scroll = (self.panels_scroll - dy).clamp(0.0, max_y),
        }
    }

    /// How far `scroller` is scrolled.
    pub(super) fn scroll_offset(&self, scroller: Scroller) -> f64 {
        match scroller {
            Scroller::ChangesList => self.changes_scroll,
            Scroller::Diff => self.diff_scroll,
            Scroller::HistoryList => self.history_scroll,
            Scroller::Detail => self.detail_scroll,
            Scroller::Panels => self.panels_scroll,
        }
    }

    pub(super) fn set_scroll_offset(&mut self, scroller: Scroller, offset: f64) {
        let slot = match scroller {
            Scroller::ChangesList => &mut self.changes_scroll,
            Scroller::Diff => &mut self.diff_scroll,
            Scroller::HistoryList => &mut self.history_scroll,
            Scroller::Detail => &mut self.detail_scroll,
            Scroller::Panels => &mut self.panels_scroll,
        };
        *slot = offset;
        self.bar_fade.wake(ui::time::now());
    }

    /// Whether the bars stay shown: a thumb is held, or the pointer is on a
    /// bar's track.
    pub(super) fn bars_held(&self) -> bool {
        self.scrollbar.is_active()
            || self.pointer.is_some_and(|p| {
                self.scroll_bars
                    .iter()
                    .any(|(_, track, _)| track.inflate(4.0, 0.0).contains(p))
            })
    }

    /// Fade each region's content out at an edge more of it can scroll in
    /// from, so a list or a diff cut by its edge shows that it goes on.
    pub(super) fn paint_edge_fades(&self, ctx: &mut PaintCtx<'_>) {
        use ui::surface::{ContentEdge, content_edge_fade};
        for region in &self.scroll_regions {
            let offset = self.scroll_offset(region.scroller);
            let mut edges = Vec::new();
            if offset > 0.5 {
                edges.push(ContentEdge::Top);
            }
            if region.max_y - offset > 0.5 {
                edges.push(ContentEdge::Bottom);
            }
            // The diff's line numbers stand still while its code scrolls
            // sideways, so only the trailing edge fades.
            if region.scroller == Scroller::Diff && region.max_x - self.diff_hscroll > 0.5 {
                edges.push(ContentEdge::Trailing);
            }
            for edge in edges {
                content_edge_fade(ctx.scene, ctx.transform, region.rect, edge, region.ground);
            }
        }
    }

    /// The edge fades, then the toolkit scroll thumb along the trailing edge
    /// of every region whose content overflows, shown while it scrolls or is
    /// pointed at and faded out at rest; records the tracks for the pointer.
    pub(super) fn paint_scroll_bars(&mut self, ctx: &mut PaintCtx<'_>) {
        self.paint_edge_fades(ctx);
        // The toolkit scroll view's bar: a thumb `s2` thick, held half an
        // `s1` off the region's edges.
        let s = &ctx.theme.tokens.spacing;
        let thickness = f64::from(s.s2);
        let margin = f64::from(s.s1) / 2.0;
        self.scroll_bars = self
            .scroll_regions
            .iter()
            .filter_map(|region| {
                let r = region.rect;
                let metrics = ScrollMetrics::new(
                    r.height() + region.max_y.max(0.0),
                    r.height(),
                    self.scroll_offset(region.scroller),
                );
                metrics.overflows().then(|| {
                    (
                        region.scroller,
                        Rect::new(
                            r.x1 - margin - thickness,
                            r.y0 + margin,
                            r.x1 - margin,
                            r.y1 - margin,
                        ),
                        metrics,
                    )
                })
            })
            .collect();
        let opacity = self.bar_fade.opacity_at(ui::time::now(), self.bars_held());
        if opacity <= 0.003 {
            return;
        }
        for &(scroller, track, metrics) in &self.scroll_bars {
            let bar = if self.scrollbar_region == Some(scroller) {
                self.scrollbar
            } else {
                Scrollbar::vertical()
            };
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                opacity,
                ctx.transform,
                &track,
            );
            bar.draw(ctx.scene, ctx.transform, track, metrics, ctx.theme, false);
            ctx.scene.pop_layer();
        }
    }

    /// A press on a scroll bar: the thumb starts a drag, the track pages.
    /// Returns whether the press was the scroll bar's.
    pub(super) fn scroll_bar_press(&mut self, pos: Point) -> bool {
        let Some(&(scroller, track, metrics)) = self
            .scroll_bars
            .iter()
            .rev()
            .find(|(_, track, _)| track.contains(pos))
        else {
            return false;
        };
        let mut bar = Scrollbar::vertical();
        match bar.press(track, metrics, pos) {
            PressOutcome::Thumb => {
                self.scrollbar = bar;
                self.scrollbar_region = Some(scroller);
                true
            }
            PressOutcome::Page(offset) => {
                self.set_scroll_offset(scroller, offset);
                true
            }
            PressOutcome::Miss => false,
        }
    }

    /// Pointer motion over the scroll bars: a held thumb follows it, and the
    /// thumb under it brightens.
    pub(super) fn scroll_bar_move(&mut self, pos: Point) {
        if self.scrollbar.is_dragging() {
            let held = self
                .scroll_bars
                .iter()
                .find(|(scroller, _, _)| Some(*scroller) == self.scrollbar_region)
                .copied();
            if let Some((scroller, track, metrics)) = held
                && let Some(offset) = self.scrollbar.drag_to(track, metrics, pos)
            {
                self.set_scroll_offset(scroller, offset);
            }
            return;
        }
        let hovered = self
            .scroll_bars
            .iter()
            .rev()
            .find(|(_, track, metrics)| {
                Scrollbar::vertical()
                    .thumb_rect(*track, *metrics)
                    .is_some_and(|thumb| thumb.contains(pos))
            })
            .map(|(scroller, _, _)| *scroller);
        self.scrollbar.set_hover(hovered.is_some());
        if hovered.is_some() {
            // Leaving the bar lets it rest a moment before it fades.
            self.bar_fade.wake(ui::time::now());
        }
        self.scrollbar_region = hovered;
    }
}
