//! The reader's whole-page bitmaps (pages and rail thumbnails), as the PDF
//! worker delivered them.
//!
//! Drawing never waits for a bitmap: a page is painted from the best one held
//! for it — the width it wants if it is here, otherwise the nearest other
//! width scaled to fit — while the wanted one renders on the worker. A
//! bitmap is therefore kept until a newer one has replaced it on screen, and
//! eviction is byte-bounded least-recently-drawn, never touching a bitmap
//! drawn in the current frame.

use std::collections::HashMap;

use vello::peniko::ImageData as PenikoImage;

/// A displayed page (0-based) and the width bucket it was rendered at.
pub type PageKey = (usize, u32);

/// Bytes of bitmaps the reader keeps. The pixels are the worker cache's own
/// buffers (shared, not copied), so this costs nothing beyond what the
/// worker's cache of the same size already holds.
pub const MAX_PAGE_IMAGE_BYTES: usize = 192 * 1024 * 1024;

struct Held {
    image: PenikoImage,
    bytes: usize,
    /// The frame it was last drawn (or delivered) in.
    used: u64,
}

pub struct PageImages {
    held: HashMap<PageKey, Held>,
    bytes: usize,
    budget: usize,
    frame: u64,
}

impl PageImages {
    pub fn new(budget: usize) -> Self {
        Self {
            held: HashMap::new(),
            bytes: 0,
            budget,
            frame: 0,
        }
    }

    /// Start a frame: what it draws is protected from eviction until the
    /// next one starts.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn contains_key(&self, key: &PageKey) -> bool {
        self.held.contains_key(key)
    }

    /// The bitmap at exactly `key`, marked drawn.
    pub fn get(&mut self, key: &PageKey) -> Option<PenikoImage> {
        let frame = self.frame;
        self.held.get_mut(key).map(|held| {
            held.used = frame;
            held.image.clone()
        })
    }

    /// The bitmap to paint `page` with when it wants `bucket`: that one if it
    /// is here, else the smallest wider one, else the widest narrower one.
    /// Marked drawn.
    pub fn best(&mut self, page: usize, bucket: u32) -> Option<(u32, PenikoImage)> {
        let key = self
            .held
            .keys()
            .filter(|(p, _)| *p == page)
            .map(|(_, b)| *b)
            .min_by_key(|b| {
                if *b >= bucket {
                    (0, *b - bucket)
                } else {
                    (1, bucket - *b)
                }
            })?;
        self.get(&(page, key)).map(|image| (key, image))
    }

    pub fn insert(&mut self, key: PageKey, image: PenikoImage) {
        let bytes = (image.width as usize)
            .saturating_mul(image.height as usize)
            .saturating_mul(4);
        self.remove(&key);
        while self.bytes.saturating_add(bytes) > self.budget {
            let victim = self
                .held
                .iter()
                .filter(|(_, held)| held.used != self.frame)
                .min_by_key(|(_, held)| held.used)
                .map(|(key, _)| *key);
            let Some(victim) = victim else {
                // Everything held is on screen this frame: keep it, and the
                // newcomer too; the next frame's eviction settles the budget.
                break;
            };
            self.remove(&victim);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.held.insert(
            key,
            Held {
                image,
                bytes,
                used: self.frame,
            },
        );
    }

    fn remove(&mut self, key: &PageKey) {
        if let Some(held) = self.held.remove(key) {
            self.bytes = self.bytes.saturating_sub(held.bytes);
        }
    }

    pub fn clear(&mut self) {
        self.held.clear();
        self.bytes = 0;
    }

    pub fn keys(&self) -> impl Iterator<Item = &PageKey> {
        self.held.keys()
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32) -> PenikoImage {
        ui::raster::rgba8(vec![0u8; (width * width * 4) as usize], width, width)
    }

    #[test]
    fn a_page_paints_from_the_nearest_width_until_its_own_arrives() {
        let mut images = PageImages::new(usize::MAX);
        images.begin_frame();
        assert!(images.best(0, 48).is_none());
        images.insert((0, 24), image(24));
        images.insert((0, 96), image(96));
        images.insert((1, 48), image(48));
        assert_eq!(images.best(0, 48).map(|(b, _)| b), Some(96), "wider first");
        assert_eq!(images.best(0, 120).map(|(b, _)| b), Some(96), "then widest");
        assert_eq!(images.best(0, 24).map(|(b, _)| b), Some(24), "exact");
        assert_eq!(images.best(1, 480).map(|(b, _)| b), Some(48));
    }

    #[test]
    fn eviction_is_least_recently_drawn_and_spares_this_frame() {
        let one = 16 * 16 * 4;
        let mut images = PageImages::new(2 * one);
        images.begin_frame();
        images.insert((0, 16), image(16));
        images.begin_frame();
        images.insert((1, 16), image(16));
        images.begin_frame();
        // Page 0 is drawn this frame; page 1 was not, so it goes.
        assert!(images.get(&(0, 16)).is_some());
        images.insert((2, 16), image(16));
        assert!(images.contains_key(&(0, 16)));
        assert!(!images.contains_key(&(1, 16)));
        assert!(images.bytes() <= 2 * one);
        // Everything held drawn this frame: nothing on screen is dropped.
        assert!(images.get(&(2, 16)).is_some());
        images.insert((3, 16), image(16));
        assert!(images.contains_key(&(0, 16)) && images.contains_key(&(2, 16)));
        assert_eq!(images.len(), 3);
        // The next frame settles the budget again.
        images.begin_frame();
        images.insert((4, 16), image(16));
        assert!(images.bytes() <= 2 * one);
    }
}
