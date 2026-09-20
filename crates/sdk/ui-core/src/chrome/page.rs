//! A centered document "page" — a sheet of paper with a soft drop shadow, the
//! familiar surface for document apps (word processor, viewers). Layout is split
//! from drawing because the page's height usually depends on content laid out
//! against [`PageGeometry::content_width`] first.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

/// Where a centered page sits within a canvas, plus its inner content band.
pub struct PageGeometry {
    pub page_x: f64,
    pub page_top: f64,
    pub page_width: f64,
    pub content_x: f64,
    pub content_width: f64,
}

/// Lay out a centered page within `canvas`: `max_width` caps the page width,
/// `top_gap` is the margin above it (and the side gap when the canvas is wide),
/// and `content_margin` is the inner text margin on each side.
pub fn geometry(canvas: Rect, max_width: f64, top_gap: f64, content_margin: f64) -> PageGeometry {
    let page_width = (canvas.width() - 2.0 * top_gap).clamp(320.0, max_width);
    let page_x = canvas.x0 + ((canvas.width() - page_width) / 2.0).max(0.0);
    let page_top = canvas.y0 + top_gap;
    PageGeometry {
        page_x,
        page_top,
        page_width,
        content_x: page_x + content_margin,
        content_width: page_width - 2.0 * content_margin,
    }
}

/// Corner radius of the sheet. A page is nearly square-cornered — paper
/// is not a card.
const PAGE_RADIUS: f64 = 3.0;

/// Draw a rounded paper sheet lifted off the desk by `elevation`.
///
/// The lift comes from the shared painter, so a page is raised by the
/// same material as every other surface.
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    page: Rect,
    paper: Color,
    elevation: &crate::tokens::Shadow,
) {
    crate::surface::shadow(scene, transform, page, PAGE_RADIUS, elevation);
    scene.fill(
        Fill::NonZero,
        transform,
        paper,
        None,
        &RoundedRect::from_rect(page, PAGE_RADIUS),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_and_clamps_width() {
        let g = geometry(Rect::new(0.0, 0.0, 2000.0, 800.0), 820.0, 28.0, 64.0);
        assert_eq!(g.page_width, 820.0, "wide canvas clamps to max width");
        assert!((g.page_x - (2000.0 - 820.0) / 2.0).abs() < 0.5, "centered");
        assert_eq!(g.content_x, g.page_x + 64.0);
        assert_eq!(g.content_width, 820.0 - 128.0);

        let narrow = geometry(Rect::new(0.0, 0.0, 400.0, 800.0), 820.0, 28.0, 64.0);
        assert_eq!(
            narrow.page_width,
            400.0 - 56.0,
            "narrow canvas shrinks the page"
        );
    }
}
