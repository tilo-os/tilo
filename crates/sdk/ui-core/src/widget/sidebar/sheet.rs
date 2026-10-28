//! A painted app's inspector as a sheet up from a narrow window's foot: the
//! same [`BottomSheet`] `NavShell`'s inspector rises as, held by the drawer.

use vello::kurbo::Rect;

use super::{SidebarDrawer, SidebarSide};
use crate::widget::flap::SplitLayout;
use crate::widget::{BottomSheet, SheetDetent, SheetFrame, SheetInput};

impl SidebarDrawer {
    /// Too narrow to dock, the inspector rises as a sheet from the window's
    /// foot instead of floating beside the content: the content above it
    /// stays live (a press there is the content's, it puts nothing away)
    /// and keeps the part the sheet rests on clear ([`Self::content_rect`]).
    /// For a trailing drawer; a sidebar keeps its own narrow form.
    #[must_use]
    pub fn sheet_when_narrow(mut self) -> Self {
        self.sheet = Some(BottomSheet::new());
        self
    }

    /// Whether the drawer is a sheet now: narrow, and asked to be one.
    #[must_use]
    pub fn rises_as_sheet(&self) -> bool {
        self.sheet.is_some()
            && self.side == SidebarSide::Right
            && self.layout == SplitLayout::Overlay
    }

    /// The sheet while the drawer is one.
    #[must_use]
    pub fn sheet(&self) -> Option<&BottomSheet> {
        self.sheet.as_ref().filter(|_| self.rises_as_sheet())
    }

    /// Where the sheet rests, while the drawer is one.
    #[must_use]
    pub fn sheet_detent(&self) -> Option<SheetDetent> {
        self.sheet().map(BottomSheet::detent)
    }

    /// Rest the sheet at `detent` at once (a capture, a test).
    pub fn snap_sheet(&mut self, detent: SheetDetent) {
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.snap_to(detent);
        }
        self.recompute_rects();
    }

    /// Back's step on the sheet while it shows ([`BottomSheet::lower`]):
    /// from the whole window it comes down to half and this returns `true`;
    /// otherwise `false`, and the holder puts the drawer away.
    pub fn lower_sheet(&mut self, motion: crate::tokens::EffectiveMotion) -> bool {
        if !self.rises_as_sheet() || !self.is_open() {
            return false;
        }
        let lowered = self.sheet.as_mut().is_some_and(|sheet| sheet.lower(motion));
        if lowered {
            self.recompute_rects();
        }
        lowered
    }

    /// The sheet's own events (a drag or press on its grip, the arrow keys
    /// on it), while it shows; call [`Self::resolve`] first.
    pub fn handle_sheet_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &crate::widget::EventCtx<'_>,
        motion: crate::tokens::EffectiveMotion,
    ) -> SheetInput {
        if !self.rises_as_sheet() || !self.is_open() {
            return SheetInput::Ignored;
        }
        let Some(sheet) = self.sheet.as_mut() else {
            return SheetInput::Ignored;
        };
        let response = sheet.handle_event(event, ctx, motion);
        if matches!(response, SheetInput::Taken { moved: true }) {
            self.recompute_rects();
        }
        response
    }

    /// What routes the sheet's actions back ([`Self::perform_sheet_action`]).
    #[must_use]
    pub fn sheet_target(&self) -> &'static str {
        "inspector-sheet"
    }

    /// The sheet as a region named `name`, its grip a slider holding the
    /// detent; `None` unless it shows.
    #[must_use]
    pub fn sheet_accessibility(&self, name: &str) -> Option<crate::a11y::AccessNode> {
        let sheet = self.sheet().filter(|_| self.is_open())?;
        Some(sheet.accessibility(name, self.sheet_target()))
    }

    /// Carry out `action` on the sheet's grip, when `target` is it.
    pub fn perform_sheet_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        motion: crate::tokens::EffectiveMotion,
    ) -> bool {
        if target != self.sheet_target() || !self.rises_as_sheet() || !self.is_open() {
            return false;
        }
        let acted = self
            .sheet
            .as_mut()
            .is_some_and(|sheet| sheet.perform_action(action, motion));
        if acted {
            self.recompute_rects();
        }
        acted
    }

    /// The sheet in `outer`: it rises under the header row, the panel is
    /// what it holds under its grip, and the content keeps the part of the
    /// foot it rests on clear.
    pub(super) fn place_sheet(&mut self, outer: Rect) {
        let top = self.overlay_top(outer) + self.float_inset;
        let frame = SheetFrame {
            area: Rect::new(outer.x0, top.min(outer.y1), outer.x1, outer.y1),
            peek: self.sheet_peek,
            grip: self.sheet_grip,
            least_half: self.sheet_least_half,
        };
        let reveal = self.flap.reveal();
        let open = self.flap.is_open(self.layout);
        let placed = self
            .sheet
            .as_mut()
            .map(|sheet| sheet.place(frame, reveal, open))
            .unwrap_or_default();
        self.sidebar_rect = if placed.panel.height() > 0.0 {
            placed.content
        } else {
            Rect::ZERO
        };
        self.content_rect = Rect::new(
            outer.x0,
            outer.y0,
            outer.x1,
            (outer.y1 - placed.covered).max(outer.y0),
        );
        self.scrim_rect = Rect::ZERO;
    }
}
