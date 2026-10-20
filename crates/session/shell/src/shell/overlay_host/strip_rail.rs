//! Strip indicators, the strip ribbon and rail.

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

    /// Feed the position the ribbon and rail are about to report into
    /// [`Self::strip_indicators`], which shows them while it changes.
    pub(super) fn prepare_strip_indicators(&mut self, now: Instant) {
        let pan_settling = || {
            self.active_strip_id()
                .and_then(|id| self.group_row_ptrs.get(&id).copied())
                // SAFETY: see field doc on `group_row_ptrs`.
                .is_some_and(|ptr| unsafe { &*ptr }.direct_pan_in_progress())
        };
        if self.strip_pan_active && pan_settling() {
            return;
        }
        let ribbon = self.strip_ribbon();
        let rail = self.strip_rail();
        let strip = self.active_strip_id();
        let scroll = strip
            .and_then(|id| self.group_row_ptrs.get(&id).copied())
            // SAFETY: see field doc on `group_row_ptrs`.
            .map_or(0.0, |ptr| unsafe { &*ptr }.scroll_x());
        let position = strip_indicators::StripPosition {
            strip,
            ribbon: (ribbon.count, ribbon.focused),
            rail: (rail.count, rail.focused),
            rail_visible: rail.visible,
            scroll_half_px: half_px_steps(scroll),
            hovered: self.strip_ribbon_hover.is_some() || self.strip_rail_hover.is_some(),
        };
        if std::mem::take(&mut self.strip_pan_active) {
            self.strip_indicators.settle_after_pan(position);
        } else {
            self.strip_indicators.observe(position, now);
        }
    }

    /// The ribbon and rail's opacity at `now`, for tests that navigate and
    /// then check what shows.
    pub fn strip_indicator_alpha_at(&mut self, now: Instant, reduce_motion: bool) -> f32 {
        self.prepare_strip_indicators(now);
        self.strip_indicators_alpha(now, reduce_motion)
    }

    /// The ribbon and rail's opacity at `now`; nothing over a fullscreen
    /// window.
    pub(super) fn strip_indicators_alpha(&self, now: Instant, reduce_motion: bool) -> f32 {
        if self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().is_fullscreen())
        {
            return 0.0;
        }
        self.strip_indicators.alpha_at(now, reduce_motion)
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

    /// Where the ribbon's long segment is drawn at `now` while it slides to
    /// the strip that took focus; `None` at rest.
    pub(super) fn strip_ribbon_focus_at(&self, now: Instant) -> Option<f64> {
        self.strip_ribbon_focus.sliding_at(now)
    }

    /// Ribbon geometry for integration tests, so a scenario can click a
    /// strip segment without duplicating the anchoring maths.
    pub fn strip_ribbon_for_tests(&self) -> ui::chrome::shell::StripRibbon {
        self.strip_ribbon()
    }

    /// The rail that maps the active strip: one segment per window group,
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
    pub(super) fn strip_rail(&self) -> ui::chrome::shell::StripRail {
        let content = self.strip_indicator_content();
        let empty = ui::chrome::shell::StripRail {
            content,
            count: 0,
            focused: 0,
            visible: Vec::new(),
        };
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
            return ui::chrome::shell::StripRail {
                content,
                count: order.len(),
                focused,
                visible: Vec::new(),
            };
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
        ui::chrome::shell::StripRail {
            content,
            count: order.len(),
            focused,
            visible,
        }
    }

    /// Rail geometry for integration tests, so a scenario can click a
    /// segment without duplicating the anchoring maths.
    pub fn strip_rail_for_tests(&self) -> ui::chrome::shell::StripRail {
        self.strip_rail()
    }

    /// The group a rail segment stands for, and the strip it is on.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "every field this reads is chained from the previous lookup (tree -> ws -> \
                  sid -> group), so the borrow can't be dropped before the last one without \
                  cloning each step early for no real gain"
    )]
    pub(super) fn strip_rail_target(&self, segment: usize) -> Option<(StripId, GroupId)> {
        let chrome = self.chrome.as_ref()?;
        let st = chrome.borrow();
        let tree = st.tree();
        let ws = tree.desktop.active_workspace()?;
        let sid = tree
            .desktop
            .active_strip
            .get(&ws.id)
            .copied()
            .filter(|id| ws.strips.iter().any(|s| s.id == *id))
            .or_else(|| ws.strips.first().map(|s| s.id))?;
        let group = ws
            .strips
            .iter()
            .find(|s| s.id == sid)?
            .window_groups
            .get(segment)?
            .id;
        Some((sid, group))
    }

    /// The workspace-settings button that opens the signed invite composer.
    ///
    /// Shared by the draw pass and the click router, so the rect that
    /// paints the button is the rect that can be clicked.
    pub(super) fn ws_settings_invite_rect(&self) -> Rect {
        WorkspaceSettingsOverlay::invite_rect_for(self.ws_settings_rect())
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
