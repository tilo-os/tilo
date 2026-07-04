//! A centered document "page" — a sheet of paper with a soft drop shadow, the
//! familiar surface for document apps (word processor, viewers). Layout is split
//! from drawing because the page's height usually depends on content laid out
//! against [`PageGeometry::content_width`] first.

use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use vello::Scene;

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

/// Draw a rounded paper sheet with a soft drop shadow.
pub fn draw(scene: &mut Scene, transform: Affine, page: Rect, paper: Color, shadow: Color) {
    let sh = Rect::new(page.x0 - 1.0, page.y0 + 4.0, page.x1 + 1.0, page.y1 + 6.0);
    scene.fill(Fill::NonZero, transform, shadow, None, &RoundedRect::from_rect(sh, 4.0));
    scene.fill(Fill::NonZero, transform, paper, None, &RoundedRect::from_rect(page, 3.0));
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
        assert_eq!(narrow.page_width, 400.0 - 56.0, "narrow canvas shrinks the page");
    }
}
