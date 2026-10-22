//! What a frame that only scrolled the document repaints.

use vello::kurbo::{Affine, Point, Rect};

use super::PdfApp;

/// How one frame showed the document canvas: what a later frame compares to
/// decide whether it only scrolled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CanvasShown {
    pub(super) scroll: (f64, f64),
    pub(super) zoom: f64,
    pub(super) canvas: Rect,
    pub(super) rect: Rect,
    pub(super) transform: Affine,
    pub(super) current_page: usize,
    /// The vertical and horizontal scroll bar thumbs, and the page bubble,
    /// which stay put while the content moves under them.
    pub(super) thumbs: [Option<Rect>; 2],
    pub(super) bubble: Rect,
    /// Whether anything else floated over the canvas (a toast, a tooltip,
    /// a find bar, the selection's controls, an editor, the scrim).
    pub(super) floating: bool,
}

impl PdfApp {
    /// A frame that only scrolled the document (and took in pages the
    /// worker delivered) offers its host a copy of what stays on screen and
    /// damages only what changed: the strip the scroll exposed, the pages
    /// that arrived, the scroll bar and page bubble that stay put over the
    /// content. Anything else repaints the window.
    pub(super) fn scroll_damage(&self, regions: &mut Vec<Rect>) {
        if !ui::widget::Widget::needs_redraw(self) {
            return;
        }
        let whole = |regions: &mut Vec<Rect>| regions.push(self.rect);
        let (Some(drawing), Some(presented)) = (self.canvas_drawing, self.canvas_presented) else {
            return whole(regions);
        };
        let only_view = !self.dirty
            && self.drag.is_none()
            && self.pan.is_none()
            && self.rail_drag.is_none()
            && !drawing.floating
            && !presented.floating
            && drawing.zoom.to_bits() == presented.zoom.to_bits()
            && drawing.canvas == presented.canvas
            && drawing.rect == presented.rect
            && drawing.transform == presented.transform
            && drawing.current_page == presented.current_page;
        if !only_view {
            return whole(regions);
        }
        let mut still: Vec<Rect> = Vec::new();
        for frame in [&drawing, &presented] {
            for thumb in frame.thumbs.iter().flatten() {
                // The whole lane the thumb runs in, so a thumb copied with
                // the content is painted over where it lands.
                still.push(Rect::new(
                    thumb.x0.min(frame.canvas.x1 - thumb.width()) - 2.0,
                    if thumb.width() < thumb.height() {
                        frame.canvas.y0
                    } else {
                        thumb.y0 - 2.0
                    },
                    frame.canvas.x1,
                    frame.canvas.y1,
                ));
            }
            if frame.bubble.area() > 0.0 {
                still.push(frame.bubble.expand());
            }
        }
        let delta = vello::kurbo::Vec2::new(
            presented.scroll.0 - drawing.scroll.0,
            presented.scroll.1 - drawing.scroll.1,
        );
        let copied = delta != vello::kurbo::Vec2::ZERO
            && ui::scroll_copy::offer(
                self,
                drawing.canvas,
                delta,
                still.clone(),
                drawing.transform,
            );
        if delta != vello::kurbo::Vec2::ZERO && !copied {
            regions.push(drawing.canvas);
        }
        regions.extend(still);
        // Pages whose pixels arrived repaint where they are now drawn.
        let card_size = vello::kurbo::Size::new(self.pw(), self.ph()) * self.zoom();
        let unit = self.stack_unit();
        for page in &self.delivered_pages {
            let origin = self
                .viewport
                .content_point_to_screen(Point::new(0.0, *page as f64 * unit));
            let card = Rect::from_origin_size(origin, card_size).intersect(drawing.canvas);
            if card.area() > 0.0 {
                regions.push(card);
            }
        }
        // A page's bitmap also stands in for its thumbnail until one of its
        // own arrives.
        if (self.delivered_thumbs || !self.delivered_pages.is_empty()) && self.show_rail {
            regions.push(self.rail_rect);
        }
    }
}
