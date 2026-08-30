//! [`StatusLog`] — a fixed-height, auto-tailing status view.
//!
//! Renders the **tail** of a list of log lines inside a bordered, clipped
//! box: as new lines arrive the newest stay visible (like `tail -f`). It
//! is a *leaf* widget — no children — so it trivially mirrors the taffy
//! tree for the App's focusable-order traversal. It is not focusable and
//! consumes no events.
//!
//! Why a bespoke widget rather than `ui::Scrollable`? The install output
//! arrives in one synchronous batch (see [`crate::install`]) and we want
//! a bounded box that always shows the latest progress without any
//! viewport/content-size wiring. Tailing is exactly the installer's need.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// Font size for log lines.
const LINE_SIZE: f32 = 13.0;
/// Extra vertical gap between lines.
const LINE_GAP: f32 = 4.0;
/// Inner padding inside the log box.
const PAD: f64 = 8.0;

pub struct StatusLog {
    lines: Vec<String>,
    height: f32,
    rect: Rect,
}

impl StatusLog {
    /// Build a log box `height` logical px tall showing `lines` (tailed).
    pub fn new(lines: Vec<String>, height: f32) -> Self {
        Self {
            lines,
            height,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for StatusLog {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(self.height),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &shape,
        );

        // Clip glyphs to the box so long lines never bleed out.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
        let max_w = (self.rect.width() - PAD * 2.0) as f32;
        let inner_h = self.rect.height() - PAD * 2.0;

        // Tail: walk from the newest line upward, summing each line's WRAPPED
        // height (measured at the box width) until the box is full. A line that
        // wraps to N visual rows must advance y by all N — otherwise the next
        // line overdraws the wrapped continuation (the cause of the overlap).
        let mut first = self.lines.len();
        let mut used = 0.0_f64;
        for i in (0..self.lines.len()).rev() {
            let (_, h) = text_renderer.measure(&self.lines[i], LINE_SIZE, Some(max_w));
            let need = f64::from(h) + f64::from(LINE_GAP);
            if used + need > inner_h && first != self.lines.len() {
                break;
            }
            used += need;
            first = i;
        }

        let mut y = self.rect.y0 + PAD;
        for line in &self.lines[first..] {
            let (_, h) = text_renderer.measure(line, LINE_SIZE, Some(max_w));
            text_renderer.draw_text(
                scene,
                line,
                LINE_SIZE,
                theme.text_primary,
                transform * Affine::translate((self.rect.x0 + PAD, y)),
                Some(max_w),
            );
            y += f64::from(h) + f64::from(LINE_GAP);
        }
        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "StatusLog"
    }

    fn test_id(&self) -> Option<&str> {
        Some("install_status_log")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
