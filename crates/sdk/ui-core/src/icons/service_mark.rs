//! The marks of the services a room may be bridged to, drawn where a
//! person a bridge speaks for appears: a small glyph in place of words, so
//! "this came from Discord" reads at a glance without a label beside every
//! name.
//!
//! A service's mark is its own brand shape, not a lucide outline: lucide
//! carries no brand glyphs. Each is one filled path in the same 24×24 box,
//! tinted by the caller to sit with the text around it.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect};
use vello::peniko::{Color, Fill};

/// Discord's mark (the "Clyde" face), as its brand kit draws it, in a 24×24
/// box. Its eyes are holes: filled even-odd.
const DISCORD: &str = "M20.317 4.37a19.791 19.791 0 0 0-4.885-1.515.074.074 0 0 0-.079.037c-.21.375-.444.865-.608 1.25a18.27 18.27 0 0 0-5.487 0 12.64 12.64 0 0 0-.617-1.25.077.077 0 0 0-.079-.037A19.736 19.736 0 0 0 3.677 4.37a.07.07 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 0 0 .031.057 19.9 19.9 0 0 0 5.993 3.03.078.078 0 0 0 .084-.028 14.09 14.09 0 0 0 1.226-1.994.076.076 0 0 0-.041-.106 13.107 13.107 0 0 1-1.872-.892.077.077 0 0 1-.008-.128 10.2 10.2 0 0 0 .372-.292.074.074 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 0 1 .078.01c.12.098.246.198.373.292a.077.077 0 0 1-.006.127 12.299 12.299 0 0 1-1.873.892.077.077 0 0 0-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 0 0 .084.028 19.839 19.839 0 0 0 6.002-3.03.077.077 0 0 0 .032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 0 0-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z";

/// The side of the box a mark's path is drawn in.
const VIEWBOX: f64 = 24.0;

/// A service's mark, ready to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceMark {
    path: &'static str,
}

/// The mark of `service` (its lowercase id, `discord`), if the toolkit
/// knows one; a service without one is named in words instead.
#[must_use]
pub fn service_mark(service: &str) -> Option<ServiceMark> {
    match service {
        "discord" => Some(ServiceMark { path: DISCORD }),
        _ => None,
    }
}

impl ServiceMark {
    fn path(self) -> BezPath {
        BezPath::from_svg(self.path).unwrap_or_default()
    }

    /// Fill the mark centred in `rect`, as large as its shorter side
    /// allows, in `color`.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo::Affine multiplication is f64 matrix composition; it cannot panic or overflow"
    )]
    pub fn draw(self, scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
        let side = rect.width().min(rect.height());
        if side <= 0.0 {
            return;
        }
        let scale = side / VIEWBOX;
        let x = rect.x0 + (rect.width() - side) / 2.0;
        let y = rect.y0 + (rect.height() - side) / 2.0;
        let local = transform * Affine::translate((x, y)) * Affine::scale(scale);
        scene.fill(Fill::EvenOdd, local, color, None, &self.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Shape as _;

    #[test]
    fn discord_has_a_mark_that_fills_its_box_and_others_have_none() {
        let mark = service_mark("discord").expect("a mark");
        let path = mark.path();
        let bounds = path.bounding_box();
        assert!(bounds.x0 >= -0.5 && bounds.x1 <= 24.5, "{bounds:?}");
        assert!(
            bounds.width() > 23.0 && bounds.height() > 17.0,
            "{bounds:?}"
        );
        // The face and its two eyes.
        assert_eq!(
            path.elements()
                .iter()
                .filter(|el| matches!(el, vello::kurbo::PathEl::MoveTo(_)))
                .count(),
            3
        );
        assert_eq!(service_mark("slack"), None);
    }
}
