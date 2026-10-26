#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// The active workspace's active strip (its first, if none is marked).
    pub(super) fn active_strip_id(&self) -> Option<StripId> {
        let chrome = self.chrome.as_ref()?;
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop.active_workspace().and_then(|ws| {
            tree.desktop
                .active_strip
                .get(&ws.id)
                .copied()
                .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                .or_else(|| ws.strips.first().map(|s| s.id))
        })
    }

    /// The content area the strip ribbon and rail stand at the trailing
    /// edge of: the focused display's, below its bar. Every display's lane
    /// leaves [`STRIP_TRAILING_GUTTER_PX`] clear for them; the desktop's
    /// own edge is only ever the rightmost display's, wherever the person
    /// works, and its middle is on no display when they are offset.
    pub(super) fn strip_indicator_content(&self) -> Rect {
        let area = self.overlay_area();
        Rect::new(
            area.x0,
            area.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX),
            area.x1,
            area.y1,
        )
    }

    /// The strip ribbon for the active workspace, anchored to the content
    /// area's right edge. Costs no layout — it sits in
    /// [`STRIP_TRAILING_GUTTER_PX`], which the strips already leave clear.
    pub(super) fn strip_ribbon(&self) -> ui::chrome::shell::StripRibbon {
        let (count, focused) = self
            .chrome
            .as_ref()
            .map_or((0, 0), |c| c.borrow().strip_focus());
        ui::chrome::shell::StripRibbon {
            content: self.strip_indicator_content(),
            count,
            focused,
        }
    }

    /// Ribbon geometry for integration tests, so a scenario can click a
    /// strip segment without duplicating the anchoring maths.
    pub fn strip_ribbon_for_tests(&self) -> ui::chrome::shell::StripRibbon {
        self.strip_ribbon()
    }

    /// The rail that maps the active strip, and its windows in rail order:
    /// one segment per window group,
    /// the focused one marked, the ones on screen lit. Reads the live
    /// [`GroupRow`] rather than the tree — visibility is a layout question,
    /// and the tree knows only order.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "sid and order are each fully chained from tree (sid resolves the active \
                  strip, order re-reads that strip's groups); nothing to drop earlier \
                  without cloning the tree data out for no real gain, and the block already \
                  ends right after both are computed"
    )]
    pub(super) fn strip_rail_and_windows(&self) -> (ui::chrome::shell::StripRail, Vec<GroupId>) {
        let content = self.strip_indicator_content();
        let empty = (
            ui::chrome::shell::StripRail {
                content,
                count: 0,
                focused: 0,
                visible: Vec::new(),
            },
            Vec::new(),
        );
        let Some(chrome) = self.chrome.as_ref() else {
            return empty;
        };
        let (strip_id, order) = {
            let st = chrome.borrow();
            let tree = st.tree();
            let Some(sid) = tree.desktop.active_workspace().and_then(|ws| {
                tree.desktop
                    .active_strip
                    .get(&ws.id)
                    .copied()
                    .filter(|id| ws.strips.iter().any(|s| s.id == *id))
                    .or_else(|| ws.strips.first().map(|s| s.id))
            }) else {
                return empty;
            };
            let order: Vec<GroupId> = tree
                .desktop
                .active_workspace()
                .and_then(|ws| ws.strips.iter().find(|s| s.id == sid))
                .map(|s| s.window_groups.iter().map(|g| g.id).collect())
                .unwrap_or_default();
            (sid, order)
        };
        if order.is_empty() {
            return empty;
        }
        let focused = chrome
            .borrow()
            .tree()
            .desktop
            .active_group_for(strip_id)
            .and_then(|g| order.iter().position(|&id| id == g))
            .unwrap_or(0);
        let Some(&ptr) = self.group_row_ptrs.get(&strip_id) else {
            let rail = ui::chrome::shell::StripRail {
                content,
                count: order.len(),
                focused,
                visible: Vec::new(),
            };
            return (rail, order);
        };
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &GroupRow = unsafe { &*ptr };
        let onscreen = row.onscreen_group_ids();
        let visible = order
            .iter()
            .enumerate()
            .filter(|(_, id)| onscreen.contains(id))
            .map(|(i, _)| i)
            .collect();
        let rail = ui::chrome::shell::StripRail {
            content,
            count: order.len(),
            focused,
            visible,
        };
        (rail, order)
    }

    /// Rail geometry, without the windows it stands for.
    pub(super) fn strip_rail(&self) -> ui::chrome::shell::StripRail {
        self.strip_rail_and_windows().0
    }

    /// Rail geometry for integration tests, so a scenario can click a
    /// segment without duplicating the anchoring maths.
    pub fn strip_rail_for_tests(&self) -> ui::chrome::shell::StripRail {
        self.strip_rail()
    }

    /// The workspace-settings button that opens the signed invite composer.
    ///
    /// Shared by the draw pass and the click router, so the rect that
    /// paints the button is the rect that can be clicked.
    pub(super) fn ws_settings_invite_rect(&self) -> Rect {
        self.workspace_settings_overlay.invite_rect()
    }

    /// Signed-invite button geometry for integration tests.
    pub fn ws_settings_invite_rect_for_tests(&self) -> Rect {
        self.ws_settings_invite_rect()
    }

    /// Public alias for `Self::quick_settings_rect` so integration
    /// tests can compute drill-in row coordinates without duplicating
    /// the popover-rect math.
    pub fn quick_settings_rect_for_tests(&self) -> Rect {
        self.quick_settings_rect()
    }

    /// Where the palette card stands on the focused display: its left and
    /// right edges and its top (a zero-height rect; the card's height is
    /// its own). Centred on the focused display: across two, the host's own
    /// centre is the seam between them. Shared by the layout and by the
    /// Super surface's quiet stage, which places the time card under the
    /// field before the field shows.
    pub(crate) fn palette_frame(&self, text_scale: f32) -> Rect {
        let area = self.overlay_area();
        // 70% of viewport clamped to [560, 640]; 560 is the minimum that
        // still fits the filter strip's eight pills without the last one
        // bleeding into the close glyph.
        let width = ((area.width() * 0.7).clamp(560.0, 640.0) * f64::from(text_scale))
            .min((area.width() - 32.0).max(320.0));
        let x0 = (area.x0 + (area.width() - width) / 2.0).max(self.rect.x0);
        let y0 = area.y0 + 96.0;
        Rect::new(x0, y0, x0 + width, y0)
    }

    /// The command palette's laid-out card, for integration tests.
    pub fn palette_rect_for_tests(&self) -> Rect {
        self.palette.get_rect()
    }
}
