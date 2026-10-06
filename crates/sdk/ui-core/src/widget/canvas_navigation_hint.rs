//! Shared, modality-aware help overlay for pan/orbit + zoom canvases.

use crate::color_compat::ColorCompat;
use crate::interaction::{InteractionEnvironment, PointerPrecision};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

/// The primary direct-manipulation verb used by a domain canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasNavigation {
    PanZoom,
    OrbitZoom,
}

/// A quiet bottom-centred overlay that teaches canvas navigation without
/// stealing focus or pointer input.
pub struct CanvasNavigationHint;

impl CanvasNavigationHint {
    pub fn label(kind: CanvasNavigation, environment: InteractionEnvironment) -> &'static str {
        match (kind, environment.pointer_precision) {
            (CanvasNavigation::PanZoom, PointerPrecision::Coarse) => "Drag to pan · Pinch to zoom",
            (CanvasNavigation::OrbitZoom, PointerPrecision::Coarse) => {
                "Drag to orbit · Pinch to zoom"
            }
            (CanvasNavigation::PanZoom, PointerPrecision::Fine) => "Drag to pan · Scroll to zoom",
            (CanvasNavigation::OrbitZoom, PointerPrecision::Fine) => {
                "Drag to orbit · Scroll to zoom"
            }
        }
    }

    /// Standard placement inside a canvas, inset from its bottom edge.
    pub fn bounds(canvas: Rect) -> Rect {
        let width = (canvas.width() - 24.0).clamp(0.0, 244.0);
        Rect::from_center_size(
            vello::kurbo::Point::new(canvas.center().x, canvas.y1 - 34.0),
            (width, 36.0),
        )
    }

    pub fn accessibility(
        canvas: Rect,
        kind: CanvasNavigation,
        environment: InteractionEnvironment,
    ) -> crate::a11y::AccessNode {
        crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
            .with_name(Self::label(kind, environment))
            .with_bounds(Self::bounds(canvas))
    }

    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        canvas: Rect,
        kind: CanvasNavigation,
        environment: InteractionEnvironment,
    ) {
        let rect = Self::bounds(canvas);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let surface = RoundedRect::from_rect(rect, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised.with_alpha_factor(0.90),
            None,
            &surface,
        );
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo Affine composition is float matrix math; it cannot overflow or panic"
        )]
        let text_transform = transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 10.0));
        text.draw_style(
            scene,
            Self::label(kind, environment),
            theme.text_dim,
            text_transform,
            None,
            &theme.text(TextRole::Caption),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_tracks_canvas_verb_and_pointer_precision() {
        assert_eq!(
            CanvasNavigationHint::label(CanvasNavigation::PanZoom, InteractionEnvironment::POINTER),
            "Drag to pan · Scroll to zoom"
        );
        assert_eq!(
            CanvasNavigationHint::label(CanvasNavigation::OrbitZoom, InteractionEnvironment::TOUCH),
            "Drag to orbit · Pinch to zoom"
        );
    }
}
