//! The preview's pages, rendered on demand through the shared page pipeline
//! ([`tilo_page_raster`]), the one the PDF reader draws through: each frame
//! hands the renderer one batch replacing its queue (the pages on screen,
//! then thumbnails, then a screen ahead in the scroll direction and half a
//! screen behind), draws every page from the best bitmap held while the
//! sharp one renders, and draws pages zoomed past the tile threshold from
//! tiles.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use vello::kurbo::{Point, Rect};
use vello::peniko::ImageData;

use tilo_page_raster::images::{Carry, MAX_PAGE_IMAGE_BYTES, PageImages};
use tilo_page_raster::reach::{Heading, pages_in_reach, stacked_span};
use tilo_page_raster::tiles::{
    self, MAX_TILE_CACHE_BYTES, PagePlacement, PageTiles, TileCache, TileKey,
};
use tilo_page_raster::{Admission, PageKey, PageRenderer, Rendered, SUPERSAMPLE};

use crate::page_text::PageText;
use crate::pages::TypstPages;

use super::PreviewPane;

/// The bitmaps and render requests behind the preview's pages.
pub(super) struct PageRaster {
    renderer: Option<PageRenderer>,
    document: Option<Arc<TypstPages>>,
    generation: u64,
    pub(super) images: PageImages,
    pub(super) tiles: TileCache<TileKey>,
    /// Renders asked for and not yet delivered.
    pending: HashSet<PageKey>,
    /// The page renders and tiles last handed to the renderer, most
    /// important first.
    batch: Vec<PageKey>,
    tile_batch: Vec<TileKey>,
    heading: Heading,
    /// A page or thumbnail on screen is not yet drawn sharp.
    pub(super) pending_visuals: bool,
    /// Page renders delivered, in order, with the document version.
    #[cfg(test)]
    pub(super) delivered: Vec<(u64, PageKey)>,
}

impl Default for PageRaster {
    fn default() -> Self {
        Self {
            renderer: None,
            document: None,
            generation: 0,
            images: PageImages::new(MAX_PAGE_IMAGE_BYTES),
            tiles: TileCache::new(MAX_TILE_CACHE_BYTES),
            pending: HashSet::new(),
            batch: Vec::new(),
            tile_batch: Vec::new(),
            heading: Heading::default(),
            pending_visuals: false,
            #[cfg(test)]
            delivered: Vec::new(),
        }
    }
}

/// What a frame draws each page and thumbnail from.
#[derive(Default)]
pub(super) struct FramePages {
    pub(super) pages: HashMap<usize, ImageData>,
    pub(super) thumbs: HashMap<usize, ImageData>,
    pub(super) tiles: HashMap<usize, PageTiles>,
}

impl PageRaster {
    /// Show a new compile of the document. A page that looks as it did keeps
    /// its bitmaps, wherever it moved; a changed page keeps its old ones as
    /// stand-ins until it renders again, so an edit never blanks the view.
    pub(super) fn show(&mut self, document: Arc<TypstPages>) {
        if let Some(old) = &self.document {
            let mut by_look: HashMap<u128, Vec<usize>> = HashMap::new();
            for (index, look) in document.looks().iter().enumerate() {
                by_look.entry(*look).or_default().push(index);
            }
            let new_len = document.len();
            let carry: Vec<Option<Carry>> = old
                .looks()
                .iter()
                .enumerate()
                .map(|(index, look)| {
                    let same = by_look.get(look);
                    if same.is_some_and(|at| at.contains(&index)) {
                        Some(Carry::Fresh(index))
                    } else if let Some(&to) = same.and_then(|at| at.first()) {
                        Some(Carry::Fresh(to))
                    } else {
                        (index < new_len).then_some(Carry::Stale(index))
                    }
                })
                .collect();
            let at = |page: usize| carry.get(page).copied().flatten();
            self.images.carry(at);
            self.tiles.carry(|key| match at(key.page) {
                Some(Carry::Fresh(page)) => Some(TileKey { page, ..*key }),
                _ => None,
            });
        } else {
            self.images.clear();
            self.tiles.clear();
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
        self.batch.clear();
        self.tile_batch.clear();
        if let Some(renderer) = &self.renderer {
            renderer.set_source(self.generation, document.clone());
        }
        let old = self.document.replace(document);
        self.retire(old);
    }

    /// Let go of `document`, a compile no longer shown: the renderer's
    /// worker drops it, so tearing down its pages costs no frame.
    fn retire(&self, document: Option<Arc<TypstPages>>) {
        if let (Some(renderer), Some(document)) = (&self.renderer, document) {
            renderer.retire(document);
        }
    }

    /// The selectable text of `page` of the compile shown.
    /// The compile shown, by address; 0 with none.
    pub(super) fn document_address(&self) -> usize {
        self.document
            .as_ref()
            .map_or(0, |document| Arc::as_ptr(document).cast::<()>().addr())
    }

    pub(super) fn text(&self, page: usize) -> Option<&PageText> {
        self.document.as_ref().map(|document| document.text(page))
    }

    /// Show no pages.
    pub(super) fn clear(&mut self) {
        let old = self.document.take();
        self.retire(old);
        self.images.clear();
        self.tiles.clear();
        self.pending.clear();
        self.batch.clear();
        self.tile_batch.clear();
        self.pending_visuals = false;
        if let Some(renderer) = &self.renderer {
            renderer.clear_source();
        }
    }

    /// Take in the renders that finished. Returns whether any did. The
    /// renderer is connected on first use, where the window's wake scope is
    /// current, so it wakes this window when a render is ready.
    pub(super) fn pump(&mut self) -> bool {
        let renderer = self.renderer.get_or_insert_with(|| {
            let renderer = PageRenderer::new();
            renderer.set_wake(ui::wake::WakeHandle::new());
            if let Some(document) = &self.document {
                renderer.set_source(self.generation, document.clone());
            }
            renderer
        });
        let results = renderer.drain();
        let any = !results.is_empty();
        for result in results {
            match result {
                Rendered::Page { key, image } => {
                    self.pending.remove(&key);
                    #[cfg(test)]
                    self.delivered.push((self.generation, key));
                    if let Some(image) = image {
                        self.images.insert(key, image);
                    }
                }
                Rendered::Tile { key, image } => {
                    self.tile_batch.retain(|queued| *queued != key);
                    if let Some(image) = image {
                        self.tiles.insert(key, image);
                    }
                }
            }
        }
        any
    }

    /// Whether the window must keep producing frames for the renderer: it
    /// runs on the UI thread where no thread could start.
    pub(super) fn wants_frames(&self) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(PageRenderer::wants_frames)
    }

    /// Renders delivered for this preview so far.
    #[cfg(test)]
    pub(super) fn rendered(&self) -> u64 {
        self.renderer.as_ref().map_or(0, PageRenderer::rendered)
    }

    /// The pages asked of the renderer and not yet delivered.
    #[cfg(test)]
    pub(super) fn pending_pages(&self) -> std::collections::BTreeSet<usize> {
        self.pending.iter().map(|(page, _)| *page).collect()
    }

    fn request(&mut self, order: &[PageKey], tiles: Vec<TileKey>) {
        let batch = tilo_page_raster::admit(order, &self.pending, Admission::FRAME);
        if let Some(renderer) = &self.renderer {
            if batch != self.batch {
                renderer.request_pages(&batch);
                self.pending = batch.iter().copied().collect();
                self.batch = batch;
            }
            if tiles != self.tile_batch {
                renderer.request_tiles(&tiles);
                self.tile_batch = tiles;
            }
        }
        self.pending_visuals |= !self.pending.is_empty();
    }

    /// The bitmap to draw `page` with when it wants `bucket`, and its width
    /// bucket: the best held, else a stand-in from before an edit (bucket 0,
    /// never sharp).
    fn draw_from(&mut self, page: usize, bucket: u32) -> Option<(u32, ImageData)> {
        self.images
            .best(page, bucket)
            .or_else(|| self.images.stale(page).map(|image| (0, image)))
    }
}

/// The width bucket a page `width` screen pixels wide renders at.
fn page_bucket(width: f64) -> u32 {
    tilo_page_raster::bucket_for(((width * SUPERSAMPLE).round() as u32).clamp(96, 4096))
}

impl PreviewPane {
    /// The screen rect of page `i`.
    pub(super) fn page_rect(&self, i: usize) -> Option<Rect> {
        let b = self.boxes.get(i)?;
        let tl = self
            .viewport
            .content_point_to_screen(Point::new(b.x_off, b.top));
        let br = self
            .viewport
            .content_point_to_screen(Point::new(b.x_off + b.w_pt, b.top + b.h_pt));
        Some(Rect::new(tl.x, tl.y, br.x, br.y))
    }

    /// The pages whose screen rects may meet `top..=bottom`, by bisection.
    pub(super) fn pages_between(&self, top: f64, bottom: f64) -> std::ops::Range<usize> {
        let main = self.viewport.rect();
        let content_y = |y: f64| self.viewport.screen_to_content(Point::new(main.x0, y)).y;
        stacked_span(&self.tops, &self.bottoms, content_y(top), content_y(bottom))
    }

    /// Where a visible page's pixels land if it is drawn from tiles: `None`
    /// when a whole-page bitmap at this zoom stays under
    /// [`tiles::TILE_MIN_PAGE_PX`].
    fn tile_placement(&self, i: usize, rect: Rect) -> Option<PagePlacement> {
        let size = *self.page_pt.get(i)?;
        let z = self.viewport.zoom();
        let device = z * SUPERSAMPLE;
        if size.0 * device <= tiles::TILE_MIN_PAGE_PX || !(size.0 * device).is_finite() {
            return None;
        }
        let level = tiles::level_for(device);
        let scale = f64::from(tiles::level_scale(level));
        Some(PagePlacement {
            page: i,
            size,
            level,
            origin: (rect.x0, rect.y0),
            screen_per_px: z / scale,
        })
    }

    /// This frame's pages and thumbnails, from what the renderer delivered,
    /// and the renderer's next batch. Nothing here waits: a page is drawn
    /// from the best bitmap held for it while the sharp one renders.
    pub(super) fn plan_frame(&mut self, thumb_slots: &[(Rect, Rect)]) -> FramePages {
        let mut frame = FramePages::default();
        self.raster.pending_visuals = false;
        if self.raster.document.is_none() {
            return frame;
        }
        self.raster.images.begin_frame();
        let main = self.viewport.rect();
        self.raster.heading.follow(self.viewport.scroll_y());
        let (below, above) = self.raster.heading.reach(main.height());

        let mut tiled = Vec::new();
        let reach = pages_in_reach(
            self.pages_between(main.y0 - above, main.y1 + below),
            (main.y0, main.y1),
            (below, above),
            |i| self.page_rect(i).map(|r| (r.y0, r.y1)),
            |i, _| match self
                .page_rect(i)
                .and_then(|rect| self.tile_placement(i, rect))
            {
                Some(placement) => {
                    tiled.push(placement);
                    true
                }
                None => false,
            },
        );
        let bucket =
            |pane: &Self, i: usize| page_bucket(pane.page_rect(i).map_or(0.0, |r| r.width()));

        let mut wanted: Vec<PageKey> = Vec::new();
        for (i, _) in &reach.visible {
            let want = bucket(self, *i);
            let held = self.raster.draw_from(*i, want);
            if let Some((_, image)) = &held {
                frame.pages.insert(*i, image.clone());
            }
            if held.is_none_or(|(at, _)| at != want) {
                wanted.push((*i, want));
            }
        }
        let placeholder = tilo_page_raster::bucket_for(tiles::PLACEHOLDER_PAGE_PX);
        for placement in &tiled {
            let i = placement.page;
            let held = self.raster.draw_from(i, u32::MAX);
            if let Some((_, image)) = &held {
                frame.pages.insert(i, image.clone());
            }
            // A thumbnail is only a stand-in; ask for the real placeholder.
            if held.is_none_or(|(at, _)| at < placeholder) {
                wanted.push((i, placeholder));
            }
            frame.tiles.insert(i, PageTiles::new(*placement));
        }
        self.raster.pending_visuals = !wanted.is_empty();

        let mut thumbs = Vec::new();
        for (i, (slot, _)) in thumb_slots.iter().enumerate() {
            if slot.y0 > self.rect.y1 || slot.y1 < self.rect.y0 {
                continue;
            }
            let want = tilo_page_raster::bucket_for(
                ((slot.width() * 2.0).round() as u32).clamp(96, tiles::THUMB_MAX_PX),
            );
            let held = self.raster.draw_from(i, want);
            match &held {
                Some((_, image)) => {
                    frame.thumbs.insert(i, image.clone());
                }
                None => self.raster.pending_visuals = true,
            }
            if held.is_none_or(|(at, _)| at < want) {
                thumbs.push((i, want));
            }
        }

        let mut ahead = Vec::new();
        for (i, _) in &reach.prefetch {
            let want = bucket(self, *i);
            if !self.raster.images.contains_key(&(*i, want)) {
                // Marked drawn so the bitmap the scroll is about to need is
                // not the one evicted for it.
                let _ = self.raster.draw_from(*i, want);
                ahead.push((*i, want));
            }
        }

        let view = (main.x0, main.y0, main.x1, main.y1);
        let wanted_tiles = tiles::wanted_tiles(&tiled, view);
        let plan = tiles::plan_tiles(
            &wanted_tiles,
            &mut self.raster.tiles,
            view,
            &mut frame.tiles,
            0,
        );
        self.raster.pending_visuals |= plan.missing;

        let mut order = wanted;
        order.extend(thumbs);
        order.extend(ahead);
        self.raster.request(&order, plan.batch);
        frame
    }
}
