//! Pane geometry shared by drawing, hit-testing, and accessibility routing.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// Computed region rectangles for one frame, derived purely from the root
/// rect. Recomputed in both `draw` and `handle_event` so the two never
/// disagree about where anything is.
#[derive(Clone, Copy, Default)]
pub(super) struct Regions {
    pub(super) bin: Rect,
    pub(super) preview: Rect,
    pub(super) canvas: Rect,
    pub(super) transport: Rect,
    pub(super) inspector: Rect,
    pub(super) timeline: Rect,
    pub(super) ruler: Rect,
    pub(super) gutter: Rect,
    pub(super) lanes: Rect,
    /// Full-bounds dimming scrim drawn under the open overlay bin. Zero-sized
    /// while docked or while the flap is fully closed.
    pub(super) scrim: Rect,
    /// `true` when the bin is collapsed to an overlay flap (narrow window).
    pub(super) bin_overlay: bool,
    /// `true` when the inspector is collapsed to an overlay flap. It slides
    /// from the right and paints no scrim — see [`INSPECTOR_BREAKPOINT`].
    pub(super) inspector_overlay: bool,
}

impl VideoEditorApp {
    #[allow(
        clippy::too_many_lines,
        reason = "the coupled editor pane geometry must remain in one coordinate calculation to prevent gaps and overlaps"
    )]
    pub(super) fn regions(&self) -> Regions {
        let r = self.rect;
        let layout = self.bin_layout();
        let overlay = layout == SplitLayout::Overlay;
        let timeline_top = self.timeline_top(r.y0, r.y1);
        let timeline = Rect::new(r.x0, timeline_top, r.x1, r.y1);
        let mid_top = r.y0;
        let mid_bot = timeline.y0;
        // Docked: the inspector holds its column and the preview ends at it.
        // Overlay: the preview reclaims the column and the inspector slides in
        // over it from the right by the eased reveal. `preview_x1` is where
        // the preview (and so the transport) actually ends, which is the whole
        // window's edge in the overlay case — that is what gives the transport
        // room for its five buttons and its timecode on a narrow window.
        let inspector_overlay = self.inspector_split() == SplitLayout::Overlay;
        let inspector_w = self.inspector_width();
        let (inspector, preview_x1) = if inspector_overlay {
            // Summoned, it floats a step inside the window on the floating
            // material, and slides in by the eased reveal.
            let reveal = self.inspector_flap.reveal();
            let w = inspector_w.min((r.width() - 2.0 * S2).max(0.0));
            let x1 = r.x1 - S2 + (w + S2) * (1.0 - reveal);
            (Rect::new(x1 - w, mid_top + S2, x1, r.y1 - S2), r.x1)
        } else {
            // Docked, it holds its column while shown and gives it back to
            // the preview as it is hidden.
            let w = inspector_w * self.inspector_flap.reveal();
            let i = Rect::new(r.x1 - w, mid_top, r.x1, mid_bot);
            (i, i.x0)
        };
        // Docked: the bin occupies its column and the preview starts after it.
        // Overlay: the preview reclaims the bin's column (full editor width)
        // and the bin slides in from the left over a scrim by the eased
        // reveal — exactly the flap kernel's overlay geometry.
        let (bin, preview_x0, scrim) = if overlay {
            // A drawer: the floating material a step inside the window,
            // sliding in from the leading edge by the eased reveal.
            let reveal = self.bin_flap.reveal();
            let bin_w = self.bin_width().min((r.width() - 2.0 * S2).max(0.0));
            let bin_x0 = r.x0 + S2 - (bin_w + S2) * (1.0 - reveal);
            let bin = Rect::new(bin_x0, mid_top + S2, bin_x0 + bin_w, r.y1 - S2);
            let scrim = if self.bin_flap.reveal() > 0.001 {
                Rect::new(r.x0, mid_top, r.x1, r.y1)
            } else {
                Rect::ZERO
            };
            (bin, r.x0, scrim)
        } else {
            let w = self.bin_width() * self.bin_flap.reveal();
            let bin = Rect::new(r.x0, mid_top, r.x0 + w, mid_bot);
            (bin, bin.x1, Rect::ZERO)
        };
        let preview = Rect::new(preview_x0, mid_top, preview_x1, mid_bot);
        let transport = Rect::new(
            preview.x0,
            preview.y1 - self.transport_height(),
            preview.x1,
            preview.y1,
        );
        let inset = S2 * self.chrome_scale();
        let canvas_area = Rect::new(
            preview.x0 + inset,
            preview.y0 + inset,
            preview.x1 - inset,
            transport.y0 - inset,
        );
        let canvas = fit_aspect(
            canvas_area,
            f64::from(self.project.width.max(16)),
            f64::from(self.project.height.max(9)),
        );
        let gutter_w = self.gutter_width().min(timeline.width());
        let ruler = Rect::new(
            timeline.x0 + gutter_w,
            timeline.y0,
            timeline.x1,
            timeline.y0 + self.ruler_height(),
        );
        let gutter = Rect::new(
            timeline.x0,
            timeline.y0 + self.ruler_height(),
            timeline.x0 + gutter_w,
            timeline.y1,
        );
        let lanes = Rect::new(
            timeline.x0 + gutter_w,
            timeline.y0 + self.ruler_height(),
            timeline.x1,
            timeline.y1,
        );
        Regions {
            bin,
            preview,
            canvas,
            transport,
            inspector,
            timeline,
            ruler,
            gutter,
            lanes,
            scrim,
            bin_overlay: overlay,
            inspector_overlay,
        }
    }
}
