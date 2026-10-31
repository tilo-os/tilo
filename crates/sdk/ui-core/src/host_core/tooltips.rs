//! An app host's hover tooltips: the widget under the pointer offers one,
//! the host feeds the dwell from its own input, and the frame paints the chip
//! over everything. Both hosts drive the same layer from here.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::tooltip::{TooltipHost, TooltipSide};

/// Every tooltip `widget` and the shown widgets under `point` within it offer
/// there. A child whose rect misses the point is passed over with all it
/// holds, except one without a rect of its own.
pub(super) fn targets_under(
    widget: &dyn Widget,
    point: Point,
    out: &mut Vec<(Rect, String, TooltipSide)>,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    out.extend(crate::widget::tooltip::placed_target_at(widget, point));
    for child in widget.children() {
        let rect = child.get_rect();
        if rect.contains(point) || rect.area() <= 0.0 {
            targets_under(child, point, out);
        }
    }
}

/// What a host keeps of its tooltip layer between frames.
#[derive(Debug, Default)]
pub(crate) struct TooltipLayer {
    host: TooltipHost,
    /// A shown chip went or came outside a frame: the next one repaints.
    redraw: bool,
    /// The resolved text and chip of the tooltip the last frame painted.
    drawn: Option<(String, Rect)>,
}

impl TooltipLayer {
    /// The frame demand of the layer: a repaint it owes, the time a
    /// pending tooltip's dwell runs out, and whether a chip is arriving.
    pub(crate) fn schedule(&self) -> (bool, Option<crate::time::Instant>, bool) {
        (self.redraw, self.host.reveal_at(), self.host.is_moving())
    }

    /// Feed one host event to the layer, before it is dispatched: with
    /// `target`, what the surface offers at the pointer's new place, the
    /// pointer settles over it; acting (a press, a key, a wheel, a touch) or
    /// leaving the surface hides the tooltip as the live window does.
    pub(crate) fn track(&mut self, event: &WindowEvent, target: Option<(Rect, String)>) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.on_pointer(target, crate::time::now());
        } else if TooltipHost::dismissed_by(event) {
            self.dismiss();
        }
    }

    /// Run the layer's clock up to now: a pending tooltip's dwell runs out
    /// and the chip it was showing arrives. Read [`Self::visible`] after it.
    pub(crate) fn tick(&mut self) {
        self.host.tick(crate::time::now());
    }

    /// Paint the shown tooltip into the frame's scene, read again from the
    /// widget under it (`current`, with the side it prefers): a control
    /// rebuilt since the pointer settled shows what it says now. Returns the
    /// resolved text, the chip, every pixel it may paint and whether it is on
    /// screen yet.
    pub(crate) fn paint(
        &mut self,
        current: Option<(Rect, String, TooltipSide)>,
        text: &mut TextRenderer,
        theme: &Theme,
        scene: &mut Scene,
        transform: Affine,
        viewport: Rect,
    ) -> Option<(String, Rect, Rect, bool)> {
        self.redraw = false;
        self.drawn = None;
        let painted = self
            .host
            .paint_shown(current, text, theme, scene, transform, viewport);
        // Only what this frame put on screen.
        if let Some((text, chip, _, true)) = painted.as_ref() {
            self.drawn = Some((text.clone(), *chip));
        }
        painted
    }

    /// The painted tooltip: its text and where its chip stands (logical).
    pub(crate) fn drawn(&self) -> Option<(&str, Rect)> {
        self.drawn
            .as_ref()
            .map(|(text, chip)| (text.as_str(), *chip))
    }

    pub(crate) fn visible(&self) -> Option<(&str, Rect)> {
        self.host.visible()
    }

    #[cfg(feature = "native")]
    pub(crate) fn needs_frame(&self) -> bool {
        self.redraw || self.host.needs_frame()
    }

    #[cfg(feature = "native")]
    pub(crate) fn reveal_at(&self) -> Option<crate::time::Instant> {
        self.host.reveal_at()
    }

    /// Hide the shown tooltip, if any. Whether it went, so the frame it went
    /// in repaints.
    pub(crate) fn dismiss(&mut self) -> bool {
        let went = self.host.dismiss();
        self.redraw |= went;
        went
    }

    /// Settle the pointer over `target`, or off any tooltip, at `now`.
    pub(crate) fn on_pointer(
        &mut self,
        target: Option<(Rect, String)>,
        now: crate::time::Instant,
    ) -> bool {
        let changed = self.host.on_pointer(target, now);
        self.redraw |= changed;
        changed
    }
}
