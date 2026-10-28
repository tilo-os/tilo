//! The inspector: shown or hidden per window size class, docked beside the
//! content, and in a window too narrow to dock it a sheet up from the
//! window's foot ([`crate::widget::BottomSheet`]) whose grip drags it
//! between its detents while the content above stays live.

use std::rc::Rc;

use vello::kurbo::{Point, Rect, Vec2};
use winit::event::WindowEvent;

use super::window_toolbar::Requests;
use super::{Column, Frame, InspectorPlacement, NavGeometry, NavShell};
use crate::widget::SheetDetent;
use crate::widget::flap::SplitLayout;

/// Where the inspector stands this frame, in the window's coordinates: the
/// one frame a retained inspector is laid out in and a painted one draws
/// its body into, docked beside the content or, in a window too narrow to
/// dock it, as the sheet up from the window's foot
/// ([`NavShell::inspector_frame`], [`InspectorLink::frame`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InspectorFrame {
    pub placement: InspectorPlacement,
    /// Whether it is open or opening.
    pub open: bool,
    /// Where its body is laid out, under its header: the docked column, or
    /// the sheet under its grip as tall as it is laid out (at half height
    /// while it peeks, at the whole window while it moves), so it reflows
    /// once a detent is reached, never on every frame. `Rect::ZERO`: not
    /// laid out.
    pub body: Rect,
    /// The part of [`Self::body`] in view: all of it docked; the sheet's
    /// at its current height, clipped at the window's foot.
    pub shown: Rect,
    /// How far its slide or rise has come, `0..=1`.
    pub reveal: f64,
    /// Where the sheet rests or is going, while the inspector is one.
    pub detent: Option<SheetDetent>,
}

/// A handle on the shell's inspector for an app painting inside it, as
/// [`super::WindowBarLink`] is for the bar's slots: what
/// [`NavShell::inspector_frame`] last laid out, and requests the shell runs
/// at its next sync or update.
#[derive(Clone)]
pub struct InspectorLink(pub(super) Rc<Requests>);

impl InspectorLink {
    /// The inspector's frame as the shell last laid it out.
    #[must_use]
    pub fn frame(&self) -> InspectorFrame {
        self.0.inspector_frame.get()
    }

    pub fn open(&self) {
        self.0.inspector_wanted.set(Some(true));
    }

    pub fn close(&self) {
        self.0.inspector_wanted.set(Some(false));
    }

    pub fn toggle(&self) {
        self.0.inspector.set(true);
    }

    /// Send the sheet to `detent` ([`NavShell::set_inspector_detent`]).
    pub fn set_detent(&self, detent: SheetDetent) {
        self.0.inspector_detent.set(Some(detent));
    }
}

pub(super) fn inspector_layout_of(inspector: InspectorPlacement) -> SplitLayout {
    match inspector {
        InspectorPlacement::Sheet => SplitLayout::Overlay,
        InspectorPlacement::Docked | InspectorPlacement::None => SplitLayout::Docked,
    }
}

/// The content above the sheet keeps the part of the window's foot the
/// sheet rests on clear, so all of it can be scrolled into view above it.
pub(super) fn keep_clear(g: &mut NavGeometry, h: f64) {
    if g.sheet_covered <= 0.0 {
        return;
    }
    let foot = h - g.sheet_covered;
    for column in [Column::Overview, Column::Content] {
        if let Some(body) = g.bodies.get_mut(column.index()) {
            body.y1 = body.y1.min(foot).max(body.y0);
        }
    }
    if g.rail.height() > 0.0 {
        g.rail.y1 = g.rail.y1.min(foot).max(g.rail.y0);
    }
}

impl NavShell {
    /// Whether the inspector shows.
    pub fn is_inspector_open(&self) -> bool {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return false,
            placement => inspector_layout_of(placement),
        };
        self.inputs.borrow().inspector.is_open(layout)
    }

    /// Show or hide the inspector, remembering the choice for this window
    /// size class (also while the app takes the inspector away and gives it
    /// back).
    pub fn toggle_inspector(&mut self) {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return,
            placement => inspector_layout_of(placement),
        };
        self.inputs.borrow_mut().inspector.toggle(layout);
        self.after_flap_change();
    }

    pub fn open_inspector(&mut self) {
        if !self.is_inspector_open() {
            self.toggle_inspector();
        }
    }

    pub fn close_inspector(&mut self) {
        if self.is_inspector_open() {
            self.toggle_inspector();
        }
    }

    /// Where the inspector stands this frame ([`InspectorFrame`]).
    #[must_use]
    pub fn inspector_frame(&self) -> InspectorFrame {
        let g = &self.geometry;
        let origin = Vec2::new(self.rect.x0, self.rect.y0);
        let body = g.body(Column::Inspector);
        let shown = if body.area() > 0.0 {
            body.intersect(g.inspector_column)
        } else {
            Rect::ZERO
        };
        let at = |rect: Rect| {
            if rect.area() > 0.0 {
                rect + origin
            } else {
                Rect::ZERO
            }
        };
        let reveal = match g.inspector {
            InspectorPlacement::None => 0.0,
            placement => self
                .inputs
                .borrow()
                .inspector
                .reveal_in(inspector_layout_of(placement)),
        };
        InspectorFrame {
            placement: g.inspector,
            open: self.is_inspector_open(),
            body: at(body),
            shown: at(shown),
            reveal,
            detent: self.inspector_detent(),
        }
    }

    /// The handle an app painting inside the shell reads the inspector's
    /// frame from and asks it to open, close or move through
    /// ([`InspectorLink`]).
    #[must_use]
    pub fn inspector_link(&self) -> InspectorLink {
        InspectorLink(Rc::clone(&self.requests))
    }

    /// Keep the frame where [`InspectorLink::frame`] reads it.
    pub(super) fn publish_inspector_frame(&self) {
        self.requests.inspector_frame.set(self.inspector_frame());
    }

    /// Run what an [`InspectorLink`] asked for. Returns whether it did
    /// anything.
    pub(super) fn take_inspector_requests(&mut self) -> bool {
        let mut acted = false;
        if let Some(open) = self.requests.inspector_wanted.take()
            && open != self.is_inspector_open()
        {
            self.toggle_inspector();
            acted = true;
        }
        if let Some(detent) = self.requests.inspector_detent.take() {
            self.set_inspector_detent(detent);
            acted = true;
        }
        acted
    }

    /// Where the inspector's sheet rests, while the inspector is one.
    #[must_use]
    pub fn inspector_detent(&self) -> Option<SheetDetent> {
        (self.geometry.inspector == InspectorPlacement::Sheet)
            .then(|| self.inputs.borrow().sheet.detent())
    }

    /// Send the inspector's sheet to `detent`, as an action asks: at once.
    /// It keeps the detent while the inspector is docked.
    pub fn set_inspector_detent(&mut self, detent: SheetDetent) {
        let motion = self.layout_theme.motion();
        self.inputs.borrow_mut().sheet.set_detent(
            detent,
            crate::tokens::MotionSource::Action,
            motion,
        );
        self.sheet_moved();
    }

    pub(super) fn sheet_shown(&self) -> bool {
        self.geometry.inspector == InspectorPlacement::Sheet && self.is_inspector_open()
    }

    /// Back's first step while the inspector is a sheet: a sheet raised to
    /// the whole window comes down to half ([`BottomSheet::lower`]), else it
    /// is put away. Returns whether there was a sheet to take the step.
    ///
    /// [`BottomSheet::lower`]: crate::widget::BottomSheet::lower
    pub(super) fn sheet_back(&mut self) -> bool {
        if !self.sheet_shown() {
            return false;
        }
        let motion = self.layout_theme.motion();
        if self.inputs.borrow_mut().sheet.lower(motion) {
            self.sheet_moved();
        } else {
            self.toggle_by_key(Column::Inspector);
        }
        true
    }

    /// Stand the sheet where this layout put it.
    pub(super) fn place_sheet(&self) {
        let shown = self.geometry.inspector == InspectorPlacement::Sheet;
        let (reveal, open) = if shown {
            let inputs = self.inputs.borrow();
            (
                inputs.inspector.reveal_in(SplitLayout::Overlay),
                inputs.inspector.is_open(SplitLayout::Overlay),
            )
        } else {
            (0.0, false)
        };
        let frame = self.geometry.sheet;
        self.inputs.borrow_mut().sheet.place(frame, reveal, open);
    }

    fn sheet_moved(&mut self) {
        self.animating = true;
        self.invalidate();
    }

    /// The sheet's own events (a drag or press on its grip, the arrow keys
    /// on it); whether it took `event`.
    pub(super) fn sheet_event(
        &mut self,
        event: &WindowEvent,
        ctx: &crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.sheet_shown() {
            return false;
        }
        let motion = self.layout_theme.motion();
        let response = self
            .inputs
            .borrow_mut()
            .sheet
            .handle_event(event, ctx, motion);
        match response {
            crate::widget::SheetInput::Ignored => false,
            crate::widget::SheetInput::Taken { moved } => {
                if moved {
                    self.sheet_moved();
                } else {
                    // The grip's focus shows.
                    self.paint_dirty = true;
                    self.invalidation.invalidate();
                }
                true
            }
        }
    }

    pub(super) fn sheet_cursor(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.sheet_shown() {
            return None;
        }
        self.inputs.borrow().sheet.cursor_shape(point)
    }

    pub(super) fn sheet_claims_touch(&self, point: Point) -> bool {
        self.sheet_shown() && self.inputs.borrow().sheet.claims_touch(point)
    }

    pub(super) fn sheet_target(&self) -> String {
        format!(
            "{}.inspector-sheet",
            self.id.as_deref().unwrap_or("nav-shell")
        )
    }

    /// The sheet as a named region, its grip a slider holding the detent.
    pub(super) fn sheet_accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.sheet_shown().then(|| {
            let name = crate::i18n::widget_format("widget-inspector", &[]);
            self.inputs
                .borrow()
                .sheet
                .accessibility(&name, &self.sheet_target())
        })
    }

    pub(super) fn perform_sheet_action(&mut self, action: &crate::a11y::Action) -> bool {
        if !self.sheet_shown() {
            return false;
        }
        let motion = self.layout_theme.motion();
        let acted = self
            .inputs
            .borrow_mut()
            .sheet
            .perform_action(action, motion);
        if acted {
            self.sheet_moved();
        }
        acted
    }

    /// The ground a floating column stands on: the sidebar's drawer, or the
    /// inspector's sheet with its grip.
    pub(super) fn paint_drawer_ground(&self, f: &mut Frame<'_>, column: Column, rect: Rect) {
        if column == Column::Inspector {
            self.inputs
                .borrow()
                .sheet
                .paint(f.scene, f.transform, f.theme);
            // The sheet's header row ends in the rule every header row
            // does, as the sidebar's sheet's does.
            let header = self.geometry.header(Column::Inspector);
            if header.height() > 0.0 {
                let local = self.column_theme(f.theme, Some(Column::Inspector));
                crate::chrome::HeaderRowFace::in_row(header, self.inputs.borrow().metrics.row)
                    .paint_rule(f.scene, f.transform, &local);
            }
        } else {
            crate::widget::sidebar::paint_floating_drawer(f.scene, f.transform, rect, f.theme);
        }
    }
}
