//! The rendered-document preview pane.
//!
//! Shows the compiled pages stacked vertically with a zoom control and, when
//! the document has more than one page, a clickable thumbnail strip. Page
//! bitmaps come from [`crate::compile::PageImage`] and are painted through the
//! toolkit's [`ui::widget::Image`] (via its `paint_at`, which positions the
//! image without a taffy pass).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Fill};

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Image, ImageFit};

use crate::compile::PageImage;

const THUMB_STRIP_W: f64 = 104.0;
const PAGE_GAP: f64 = 24.0;
const PAGE_MARGIN: f64 = 28.0;
const THUMB_GAP: f64 = 14.0;

pub struct PreviewPane {
    rect: Rect,
    images: Vec<Image>,
    /// Intrinsic page sizes in px (width, height) parallel to `images`.
    sizes: Vec<(u32, u32)>,
    zoom: f64,
    scroll_y: f64,
    /// Page index considered "current" (top-most visible) — drives the
    /// thumbnail highlight and the status bar.
    current: usize,
    /// Set by the app when there are no pages to show.
    placeholder: Option<String>,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            images: Vec::new(),
            sizes: Vec::new(),
            zoom: 1.0,
            scroll_y: 0.0,
            current: 0,
            placeholder: Some("Compiling…".into()),
        }
    }
}

impl PreviewPane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn set_pages(&mut self, pages: &[PageImage]) {
        self.images = pages
            .iter()
            .map(|p| Image::from_rgba(p.rgba.clone(), p.width, p.height).with_fit(ImageFit::Fill))
            .collect();
        self.sizes = pages.iter().map(|p| (p.width, p.height)).collect();
        self.placeholder = if pages.is_empty() {
            Some("No pages".into())
        } else {
            None
        };
        let max = self.max_scroll();
        if self.scroll_y > max {
            self.scroll_y = max;
        }
    }

    pub fn set_placeholder(&mut self, text: Option<String>) {
        if self.images.is_empty() {
            self.placeholder = text;
        }
    }

    pub fn page_count(&self) -> usize {
        self.images.len()
    }

    pub fn current_page(&self) -> usize {
        self.current
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).min(4.0);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).max(0.25);
    }

    pub fn fit_width(&mut self) {
        self.zoom = 1.0;
    }

    fn has_thumbs(&self) -> bool {
        self.images.len() > 1
    }

    /// The main (non-thumbnail) viewport.
    fn main_rect(&self) -> Rect {
        if self.has_thumbs() {
            Rect::new(self.rect.x0 + THUMB_STRIP_W, self.rect.y0, self.rect.x1, self.rect.y1)
        } else {
            self.rect
        }
    }

    /// Display width of a page at the current zoom (fit-to-width × zoom).
    fn page_display_width(&self) -> f64 {
        let avail = (self.main_rect().width() - 2.0 * PAGE_MARGIN).max(40.0);
        // Use the first page's aspect as the fit basis; "fit width" maps the
        // page to the available width at zoom 1.0.
        avail * self.zoom
    }

    fn page_display_size(&self, i: usize) -> (f64, f64) {
        let (w, h) = self.sizes.get(i).copied().unwrap_or((1, 1));
        let dw = self.page_display_width();
        let dh = dw * (h.max(1) as f64) / (w.max(1) as f64);
        (dw, dh)
    }

    fn content_height(&self) -> f64 {
        let mut y = PAGE_MARGIN;
        for i in 0..self.images.len() {
            let (_, dh) = self.page_display_size(i);
            y += dh + PAGE_GAP;
        }
        y + PAGE_MARGIN - PAGE_GAP
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.rect.height()).max(0.0)
    }

    pub fn scroll_by(&mut self, dy: f64) {
        self.scroll_y = (self.scroll_y + dy).clamp(0.0, self.max_scroll());
        self.recompute_current();
    }

    fn recompute_current(&mut self) {
        // The current page is whichever page's top is nearest above the
        // viewport's vertical centre.
        let mid = self.scroll_y + self.rect.height() * 0.35;
        let mut y = PAGE_MARGIN;
        let mut cur = 0;
        for i in 0..self.images.len() {
            let (_, dh) = self.page_display_size(i);
            if y <= mid {
                cur = i;
            }
            y += dh + PAGE_GAP;
        }
        self.current = cur;
    }

    /// Scroll so page `i` sits at the top of the viewport.
    pub fn scroll_to_page(&mut self, i: usize) {
        let mut y = PAGE_MARGIN;
        for k in 0..i.min(self.images.len()) {
            let (_, dh) = self.page_display_size(k);
            y += dh + PAGE_GAP;
        }
        self.scroll_y = (y - PAGE_MARGIN).clamp(0.0, self.max_scroll());
        self.current = i.min(self.images.len().saturating_sub(1));
    }

    /// Handle a click; returns true if it hit a thumbnail (and jumped).
    pub fn on_mouse_down(&mut self, p: Point) -> bool {
        if !self.has_thumbs() {
            return false;
        }
        let strip = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + THUMB_STRIP_W, self.rect.y1);
        if !strip.contains(p) {
            return false;
        }
        // Walk thumbnail layout to find which one was clicked.
        let mut y = self.rect.y0 + THUMB_GAP;
        let tw = THUMB_STRIP_W - 2.0 * THUMB_GAP;
        for i in 0..self.images.len() {
            let (pw, ph) = self.sizes[i];
            let th = tw * (ph.max(1) as f64) / (pw.max(1) as f64);
            let r = Rect::new(self.rect.x0 + THUMB_GAP, y, self.rect.x0 + THUMB_GAP + tw, y + th);
            if r.contains(p) {
                self.scroll_to_page(i);
                return true;
            }
            y += th + THUMB_GAP + 16.0; // + label row
        }
        true // consumed the click in the strip regardless
    }

    pub fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Pane background.
        scene.fill(Fill::NonZero, transform, c.neutral[1], None, &self.rect);

        if self.images.is_empty() {
            self.draw_placeholder(scene, transform, tr, theme);
            return;
        }

        self.draw_main(scene, transform, tr, theme);
        if self.has_thumbs() {
            self.draw_thumbs(scene, transform, tr, theme);
        }
    }

    fn draw_main(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let main = self.main_rect();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &main);

        let mut y = main.y0 + PAGE_MARGIN - self.scroll_y;
        for i in 0..self.images.len() {
            let (dw, dh) = self.page_display_size(i);
            let x = main.x0 + (main.width() - dw) * 0.5;
            let page_rect = Rect::new(x, y, x + dw, y + dh);

            // Cull pages outside the viewport.
            if page_rect.y1 >= main.y0 && page_rect.y0 <= main.y1 {
                // Drop shadow (offset behind the page).
                let shadow = Rect::new(x + 3.0, y + 4.0, x + dw + 3.0, y + dh + 4.0);
                scene.fill(Fill::NonZero, transform, c.surfaces.overlay, None, &shadow);
                // Paper.
                self.images[i].paint_at(scene, transform, page_rect);
                // Border.
                scene.stroke(&Stroke::new(1.0), transform, c.border_standard, None, &page_rect);

                // Page number caption under the page.
                let label = format!("Page {} of {}", i + 1, self.images.len());
                let ly = y + dh + 6.0;
                tr.draw_text(
                    scene,
                    &label,
                    11.0,
                    c.text_dim,
                    transform * Affine::translate((x, ly)),
                    None,
                );
            }
            y += dh + PAGE_GAP;
        }

        scene.pop_layer();
    }

    fn draw_thumbs(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let strip = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + THUMB_STRIP_W, self.rect.y1);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &strip);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &vello::kurbo::Line::new(
                Point::new(strip.x1, strip.y0),
                Point::new(strip.x1, strip.y1),
            ),
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &strip);

        let tw = THUMB_STRIP_W - 2.0 * THUMB_GAP;
        let mut y = self.rect.y0 + THUMB_GAP;
        for i in 0..self.images.len() {
            let (pw, ph) = self.sizes[i];
            let th = tw * (ph.max(1) as f64) / (pw.max(1) as f64);
            let r = Rect::new(self.rect.x0 + THUMB_GAP, y, self.rect.x0 + THUMB_GAP + tw, y + th);

            // Selection ring for the current page.
            if i == self.current {
                let ring = r.inflate(3.0, 3.0);
                scene.stroke(&Stroke::new(2.0), transform, theme.accent_primary, None, &ring);
            }
            self.images[i].paint_at(scene, transform, r);
            scene.stroke(&Stroke::new(1.0), transform, c.border_standard, None, &r);

            let label = format!("{}", i + 1);
            tr.draw_text(
                scene,
                &label,
                10.0,
                c.text_dim,
                transform * Affine::translate((r.x0, r.y1 + 2.0)),
                None,
            );
            y += th + THUMB_GAP + 14.0;
        }

        scene.pop_layer();
    }

    fn draw_placeholder(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let text = self.placeholder.clone().unwrap_or_else(|| "No output".into());
        let (w, _h) = tr.measure(&text, 14.0, None);
        let x = self.rect.x0 + (self.rect.width() - w as f64) * 0.5;
        let y = self.rect.y0 + self.rect.height() * 0.5 - 9.0;
        tr.draw_text(
            scene,
            &text,
            14.0,
            c.text_dim,
            transform * Affine::translate((x.max(self.rect.x0 + 12.0), y)),
            None,
        );
    }
}
