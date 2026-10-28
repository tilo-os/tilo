//! What a frame that only scrolled the document repaints.

use vello::kurbo::{Affine, Point, Rect, Vec2};

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
    /// The vertical and horizontal scroll bar thumbs, which stay put while
    /// the content moves under them.
    pub(super) thumbs: [Option<Rect>; 2],
    /// The selection's bar, which stands where the selection was when it
    /// was drawn while the pages scroll under it.
    pub(super) hud: Rect,
    /// Whether anything else floated over the canvas (a toast, a tooltip,
    /// a find bar, the selection's controls, an editor, the scrim).
    pub(super) floating: bool,
}

impl ui::scroll_copy::Shown for CanvasShown {
    fn scroll(&self) -> Vec2 {
        Vec2::new(self.scroll.0, self.scroll.1)
    }

    fn clip(&self) -> Rect {
        self.canvas
    }

    fn transform(&self) -> Affine {
        self.transform
    }

    fn same_view(&self, other: &Self) -> bool {
        !self.floating
            && !other.floating
            && self.zoom.to_bits() == other.zoom.to_bits()
            && self.rect == other.rect
    }

    fn overlays(&self, out: &mut Vec<Rect>) {
        for thumb in self.thumbs.iter().flatten() {
            // The whole lane the thumb runs in, so a thumb copied with the
            // content is painted over where it lands.
            out.push(Rect::new(
                thumb.x0.min(self.canvas.x1 - thumb.width()) - 2.0,
                if thumb.width() < thumb.height() {
                    self.canvas.y0
                } else {
                    thumb.y0 - 2.0
                },
                self.canvas.x1,
                self.canvas.y1,
            ));
        }
        if self.hud.area() > 0.0 {
            out.push(self.hud.expand());
        }
    }
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
        let only_view =
            !self.dirty && self.drag.is_none() && self.pan.is_none() && self.rail_drag.is_none();
        let Some(current_page) = self
            .shown
            .frames()
            .map(|(drawing, presented)| (drawing.current_page, presented.current_page))
        else {
            return regions.push(self.rect);
        };
        let mut damage = Vec::new();
        if !only_view || self.shown.compare(self, &mut damage) == ui::scroll_copy::Since::Repaint {
            return regions.push(self.rect);
        }
        regions.extend(damage);
        let Some(drawing) = self.shown.drawing() else {
            return;
        };
        // The page rail and the outline mark the current page.
        if current_page.0 != current_page.1 {
            if self.show_rail {
                regions.push(self.rail_rect);
            }
            if self.show_sidebar {
                regions.push(self.sidebar_rect);
            }
        }
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
