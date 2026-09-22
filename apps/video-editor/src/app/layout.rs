//! Pane geometry shared by drawing, hit-testing, and accessibility routing.

use super::*;

/// Computed region rectangles for one frame, derived purely from the root
/// rect. Recomputed in both `draw` and `handle_event` so the two never
/// disagree about where anything is.
#[derive(Clone, Copy, Default)]
pub(super) struct Regions {
    pub(super) topbar: Rect,
    /// Hamburger toggle in the topbar. Zero-sized (and skipped) while the bin
    /// is docked; present only in the narrow overlay layout.
    pub(super) menu_btn: Rect,
    /// Inspector toggle in the topbar. Zero-sized while the inspector is
    /// docked; present only in the narrow overlay layout.
    pub(super) inspector_btn: Rect,
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
        let topbar_target = self.topbar_target();
        let topbar_y = r.y0 + (topbar.height() - topbar_target) / 2.0;
        let topbar_edge = TOPBAR_EDGE * self.chrome_scale();
        let topbar_gap = TOPBAR_GAP * self.chrome_scale();
        let topbar_compact = self.topbar_is_compact();
        let command_w = if topbar_compact {
            topbar_target
        } else {
            TOPBAR_BTN_W
        };
        let export_btn = Rect::new(
            r.x1 - topbar_edge - command_w,
            topbar_y,
            r.x1 - topbar_edge,
            topbar_y + topbar_target,
        );
        // Directly beneath Export, in the same strip the project notice uses,
        // so the two never share a pixel — see `project_notice_limit`.
        let chip_w = EXPORT_CHIP_W.min((r.width() - 24.0).max(0.0));
        // On the pane-heading band, so the chip centres on the same line as
        // the "INSPECTOR" heading it sits beside (and the project notice
        // plate, which shares the strip).
        let export_chip = Rect::new(
            export_btn.x1 - chip_w,
            topbar.y1 + PANE_HEADING_Y0,
            export_btn.x1,
            topbar.y1 + PANE_HEADING_Y0 + PANE_HEADING_H,
        );
        let split_btn = if topbar_compact {
            Rect::ZERO
        } else {
            Rect::new(
                export_btn.x0 - topbar_gap - TOPBAR_BTN_W,
                topbar_y,
                export_btn.x0 - topbar_gap,
                topbar_y + topbar_target,
            )
        };
        let import_right = if split_btn.width() > 0.0 {
            split_btn.x0 - topbar_gap
        } else {
            export_btn.x0 - topbar_gap
        };
        let import_btn = Rect::new(
            import_right
                - if topbar_compact {
                    topbar_target
                } else {
                    TOPBAR_BTN_W
                },
            topbar_y,
            import_right,
            topbar_y + topbar_target,
        );
        // The hamburger replaces the static app glyph slot when the bin is an
        // overlay, so the user can summon the collapsed bin.
        let menu_btn = if overlay {
            Rect::new(
                r.x0 + topbar_edge,
                topbar_y,
                r.x0 + topbar_edge + topbar_target,
                topbar_y + topbar_target,
            )
        } else {
            Rect::ZERO
        };
        // Summons the collapsed inspector. It sits left of Import rather than
        // on the right with the panel it opens, because the right of the
        // topbar is already full at the widths where the panel collapses —
        // and a toggle that does not fit is worse than one on the wrong side.
        let inspector_btn = if self.inspector_split() == SplitLayout::Overlay {
            Rect::new(
                import_btn.x0 - topbar_gap - topbar_target,
                topbar_y,
                import_btn.x0 - topbar_gap,
                topbar_y + topbar_target,
            )
        } else {
            Rect::ZERO
        };
        // The project button occupies the topbar's leading area, after the
        // hamburger when there is one. It is width-bounded and left-aligned
        // rather than centred, because the centred project meta that used to
        // live here collided with the right-hand buttons at 720px — the narrow
        // capture width itself. Below the room it needs it is dropped
        // entirely: a truncation so short it reads as a different project name
        // would be worse than nothing, and the shortcuts still reach it.
        let project_x0 = if overlay {
            menu_btn.x1 + topbar_gap
        } else {
            r.x0 + topbar_edge
        };
        let leading_limit = if inspector_btn.width() > 0.0 {
            inspector_btn.x0
        } else {
            import_btn.x0
        } - topbar_gap;
        let project_btn = {
            // At a large text size a short, ellipsized title is less useful
            // than a clear project-menu affordance.  Keep the film icon and
            // its full accessible name, but give it the same compact target
            // as the two pane toggles.
            let want = if topbar_compact {
                topbar_target
            } else {
                PROJECT_BTN_W.min(leading_limit - project_x0)
            };
            if want >= PROJECT_BTN_MIN_W {
                Rect::new(
                    project_x0,
                    topbar_y,
                    project_x0 + want,
                    topbar_y + topbar_target,
                )
            } else if topbar_compact && leading_limit >= project_x0 + topbar_target {
                Rect::new(
                    project_x0,
                    topbar_y,
                    project_x0 + topbar_target,
                    topbar_y + topbar_target,
                )
            } else {
                Rect::ZERO
            }
        };
        let timeline_top = (r.y1 - self.timeline_height()).max(topbar.y1 + 80.0);
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
            let slide = self.inspector_flap.slide();
            let x1 = r.x1 + inspector_w * (1.0 - slide);
            (Rect::new(x1 - inspector_w, mid_top, x1, r.y1), r.x1)
        } else {
            let i = Rect::new(r.x1 - inspector_w, mid_top, r.x1, mid_bot);
            (i, i.x0)
        };
        // Docked: the bin occupies its column and the preview starts after it.
        // Overlay: the preview reclaims the bin's column (full editor width)
        // and the bin slides in from the left over a scrim by the eased
        // reveal — exactly the flap kernel's overlay geometry.
        let (bin, preview_x0, scrim) = if overlay {
            let slide = self.bin_flap.slide();
            let bin_w = self.bin_width();
            let bin_x0 = r.x0 - bin_w * (1.0 - slide);
            let bin = Rect::new(bin_x0, mid_top, bin_x0 + bin_w, r.y1);
            let scrim = if self.bin_flap.reveal() > 0.001 {
                Rect::new(r.x0, mid_top, r.x1, r.y1)
            } else {
                Rect::ZERO
            };
            (bin, r.x0, scrim)
        } else {
            let bin = Rect::new(r.x0, mid_top, r.x0 + self.bin_width(), mid_bot);
            (bin, bin.x1, Rect::ZERO)
        };
        let preview = Rect::new(preview_x0, mid_top, preview_x1, mid_bot);
        let transport = Rect::new(
            preview.x0,
            preview.y1 - self.transport_height(),
            preview.x1,
            preview.y1,
        );
        let canvas_area = Rect::new(
            preview.x0 + 10.0,
            preview.y0 + 10.0,
            preview.x1 - 10.0,
            transport.y0 - 8.0,
        );
        let canvas = fit_aspect(
            canvas_area,
            f64::from(self.project.width.max(16)),
            f64::from(self.project.height.max(9)),
        );
        let ruler = Rect::new(
            timeline.x0 + GUTTER_W,
            timeline.y0,
            timeline.x1,
            timeline.y0 + self.ruler_height(),
        );
        let gutter = Rect::new(
            timeline.x0,
            timeline.y0 + self.ruler_height(),
            timeline.x0 + GUTTER_W,
            timeline.y1,
        );
        let lanes = Rect::new(
            timeline.x0 + GUTTER_W,
            timeline.y0 + self.ruler_height(),
            timeline.x1,
            timeline.y1,
        );
        Regions {
            topbar,
            menu_btn,
            inspector_btn,
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
