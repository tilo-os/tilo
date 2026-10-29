//! The hover tooltips of a surface this harness hosts (the browser page's
//! desktop and windows among them): the same [`crate::widget::TooltipHost`]
//! the native `App` drives, fed from host input and drawn into the frame's
//! scene over everything but other people's pointers.

use crate::host_core::HostCore as _;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::FrameHarness;
use crate::widget::Widget;
use crate::widget::tooltip::TooltipSide;

/// Every tooltip `widget` and the shown widgets under `point` within it
/// offer there. A child whose rect misses the point is passed over with
/// all it holds, except one without a rect of its own.
fn targets_under(widget: &dyn Widget, point: Point, out: &mut Vec<(Rect, String, TooltipSide)>) {
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

/// What the tooltip layer of one harness keeps between frames.
#[derive(Debug, Default)]
pub(super) struct TooltipLayer {
    host: crate::widget::TooltipHost,
    /// A shown chip went or came outside a frame: the next one repaints.
    redraw: bool,
    /// The resolved text and chip of the tooltip the last frame painted.
    drawn: Option<(String, Rect)>,
}

impl TooltipLayer {
    /// The frame demand of the layer: a repaint it owes, the time a
    /// pending tooltip's dwell runs out, and whether a chip is arriving.
    pub(super) fn schedule(&self) -> (bool, Option<crate::time::Instant>, bool) {
        (self.redraw, self.host.reveal_at(), self.host.is_moving())
    }
}

impl FrameHarness {
    /// Feed one host event to the tooltip layer, before it is dispatched:
    /// the pointer settles over what the last frame showed, and acting (a
    /// press, a key, a wheel, a touch) or leaving the surface hides the
    /// tooltip as the live window does.
    pub(super) fn track_tooltip(&mut self, event: &WindowEvent) {
        let changed = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.input_scale;
                let point = Point::new(position.x / scale, position.y / scale);
                let was = self.tooltips.host.visible().is_some();
                let target = self
                    .tooltip_target_at(point)
                    .map(|(rect, tip, _)| (rect, tip));
                self.tooltips.host.on_pointer(target, crate::time::now());
                was != self.tooltips.host.visible().is_some()
            }
            event if crate::widget::TooltipHost::dismissed_by(event) => {
                let was = self.tooltips.host.visible().is_some();
                self.tooltips.host.dismiss();
                was
            }
            _ => false,
        };
        self.tooltips.redraw |= changed;
    }

    /// The tooltip the laid-out surface offers at logical `point`, none
    /// where the pointer cannot hover. Its laid-out nodes are not all of
    /// it: a container that places its own children (a desktop's strip of
    /// windows, a window's bar) keeps them out of the node map, so the
    /// shown tree is walked under the pointer as well.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String, TooltipSide)> {
        if !self.interaction.supports_hover() {
            return None;
        }
        let mut targets = self.tree().tooltips_at(point);
        for root in self.presentation_roots() {
            targets_under(root, point, &mut targets);
        }
        crate::widget::tooltip::smallest_target(targets)
    }
    /// Paint the shown tooltip into the frame's scene, read again from the
    /// widget under it: a control rebuilt since the pointer settled (a
    /// toggle that now reads "Show") shows what it says now.
    pub(super) fn draw_tooltip(&mut self, transform: Affine) {
        self.tooltips.redraw = false;
        self.tooltips.drawn = None;
        self.tooltips.host.tick(crate::time::now());
        let current = self
            .tooltips
            .host
            .visible()
            .and_then(|(_, anchor)| self.tooltip_target_at(anchor.center()));
        let viewport = Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        );
        let painted = self.tooltips.host.paint_shown(
            current,
            &mut self.text,
            &self.theme,
            &mut self.scene,
            transform,
            viewport,
        );
        // Only what this frame put on screen.
        if let Some((text, chip, _, true)) = painted {
            self.tooltips.drawn = Some((text, chip));
        }
    }

    /// The painted tooltip as the accessibility tree reads it: a tooltip
    /// naming what the control under the pointer does, where it shows.
    pub(super) fn tooltip_inspection(&self) -> Option<super::InspectionNode> {
        let (text, chip) = self.tooltips.drawn.as_ref()?;
        Some(super::InspectionNode {
            path: "tooltip".into(),
            widget: "Tooltip".into(),
            test_id: None,
            focusable: false,
            rect: [chip.x0, chip.y0, chip.width(), chip.height()],
            child_clip: None,
            layout: None,
            semantics: Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Tooltip).with_name(text.clone()),
            ),
            children: Vec::new(),
        })
    }

    /// The text and chip (logical) of the tooltip the last frame painted.
    #[must_use]
    pub fn drawn_tooltip(&self) -> Option<(&str, Rect)> {
        self.tooltips
            .drawn
            .as_ref()
            .map(|(text, chip)| (text.as_str(), *chip))
    }
}
