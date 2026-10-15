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
    pub(super) topbar: Rect,
    /// The media bin's show/hide toggle, at the topbar's leading corner.
    pub(super) menu_btn: Rect,
    /// The inspector's show/hide toggle, at the topbar's trailing end
    /// (before the window's History control when the row holds it).
    pub(super) inspector_btn: Rect,
    /// The window's History control, in the header row's trailing corner;
    /// zero-sized while the host offers none.
    pub(super) history_btn: Rect,
    /// The project button: what is open, whether it is saved, and the menu
    /// that opens / saves one. Zero-sized when the topbar's leading area has
    /// no room for it — a control that overlaps Import is worse than one that
    /// is only reachable by keyboard, and Ctrl+O / Ctrl+S still work.
    pub(super) project_btn: Rect,
    pub(super) export_btn: Rect,
    /// The render's progress / outcome chip, in the strip under the topbar and
    /// anchored to the Export button above it. A render outlives the dialog
    /// that starts it, so its state needs a home in the chrome that does not
    /// depend on the dialog being up. Painted and hit-tested only while
    /// [`ExportStatus`] is not `Idle`.
    pub(super) export_chip: Rect,
    pub(super) split_btn: Rect,
    pub(super) import_btn: Rect,
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
        let topbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.topbar_height());
        let target = self.topbar_target();
        let topbar_y = r.y0 + (topbar.height() - target) / 2.0;
        let edge = TOPBAR_EDGE * self.chrome_scale();
        let gap = TOPBAR_GAP * self.chrome_scale();
        let compact = self.topbar_is_compact();
        let widths = self.topbar_widths();
        let slot = |x0: f64, w: f64| Rect::new(x0, topbar_y, x0 + w, topbar_y + target);
        // The sidebar toggle at the top-leading corner and the inspector
        // toggle at the trailing one, in every layout: one way to show or
        // hide either panel, wherever the window's width puts them.
        let menu_btn = slot(r.x0 + edge, target);
        // The window's History control holds the trailing corner, where a
        // shell's header row keeps it; the inspector toggle stands before it.
        let history_btn = ui::chrome::HeaderRowFace::new(topbar, &self.chrome_theme)
            .with_history_if(self.history.is_shown())
            .history_rect();
        let trailing_x1 = if history_btn.width() > 0.0 {
            history_btn.x0 - gap
        } else {
            r.x1 - edge
        };
        let inspector_btn = slot(trailing_x1 - target, target);
        // The trailing group, right to left from the inspector toggle: the
        // commands, each as wide as its label.
        let export_w = if compact { target } else { widths.export };
        let export_btn = slot(inspector_btn.x0 - gap - export_w, export_w);
        let split_btn = if compact {
            Rect::ZERO
        } else {
            slot(export_btn.x0 - gap - widths.split, widths.split)
        };
        let import_right = if split_btn.width() > 0.0 {
            split_btn.x0 - gap
        } else {
            export_btn.x0 - gap
        };
        let import_w = if compact { target } else { widths.import };
        let import_btn = slot(import_right - import_w, import_w);
        // The project button occupies the leading area after the sidebar
        // toggle. It is width-bounded and left-aligned rather than centred,
        // and below the room it needs it is dropped entirely: a truncation so
        // short it reads as a different project name would be worse than
        // nothing, and the shortcuts still reach it.
        let project_x0 = menu_btn.x1 + gap;
        let leading_limit = import_btn.x0 - gap;
        let mut project_btn = {
            let want = if compact {
                target
            } else {
                widths.project.min(leading_limit - project_x0)
            };
            if want >= PROJECT_BTN_MIN_W.min(widths.project) {
                slot(project_x0, want)
            } else if compact && leading_limit >= project_x0 + target {
                slot(project_x0, target)
            } else {
                Rect::ZERO
            }
        };
        // The header row's middle: where the render reports itself, trailing
        // against the commands, and where the project notice runs from the
        // project button. The two never share a pixel — see
        // `project_notice_limit`.
        let middle_x0 = if project_btn.width() > 0.0 {
            project_btn.x1
        } else {
            menu_btn.x1
        } + gap;
        let middle_x1 = import_btn.x0 - gap;
        let chip_w = self
            .export_chip_width()
            .min((middle_x1 - middle_x0).max(0.0));
        let chip_h = self.topbar_small_height();
        let chip_y = topbar.center().y - chip_h / 2.0;
        let export_chip = Rect::new(middle_x1 - chip_w, chip_y, middle_x1, chip_y + chip_h);
        if project_btn.x1 > import_btn.x0 - gap {
            project_btn = Rect::ZERO;
        }
        let timeline_top = self.timeline_top(topbar.y1, r.y1);
        let timeline = Rect::new(r.x0, timeline_top, r.x1, r.y1);
        let mid_top = topbar.y1;
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
            topbar,
            menu_btn,
            inspector_btn,
            history_btn,
            project_btn,
            export_btn,
            export_chip,
            split_btn,
            import_btn,
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
