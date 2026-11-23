//! A viewer's whole-page bitmaps (pages and thumbnails), as its worker
//! delivered them.
//!
//! Drawing never waits for a bitmap: a page is painted from the best one held
//! for it — the width it wants if it is here, otherwise the nearest other
//! width scaled to fit — while the wanted one renders on the worker. A
//! bitmap is therefore kept until a newer one has replaced it on screen, and
//! eviction is byte-bounded least-recently-drawn, never touching a bitmap
//! drawn in the current frame.
//!
//! When the document changes under the viewer (an edit recompiled it), the
//! bitmaps of pages whose content is unchanged carry over as they are, and
//! those of changed pages stay as stale stand-ins drawn until the page's new
//! bitmap arrives ([`PageImages::carry`]).

use std::collections::HashMap;

use vello::peniko::ImageData as PenikoImage;

use crate::PageKey;

/// Bytes of bitmaps a viewer keeps. The PDF reader's pixels are the worker
/// cache's own buffers (shared, not copied), so this costs nothing beyond
/// what the worker's cache of the same size already holds.
pub const MAX_PAGE_IMAGE_BYTES: usize = 192 * 1024 * 1024;

struct Held {
    image: PenikoImage,
    bytes: usize,
    /// The frame it was last drawn (or delivered) in.
    used: u64,
    /// Rendered from an earlier version of the page.
    stale: bool,
}

/// Where an old page's bitmaps go when the document changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    /// The page is unchanged, now at this index.
    Fresh(usize),
    /// The page at this index changed: its old bitmaps stand in for it.
    Stale(usize),
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

    /// Whether the current bitmap at exactly `key` is held.
    pub fn contains_key(&self, key: &PageKey) -> bool {
        self.held.get(key).is_some_and(|held| !held.stale)
    }

    /// The current bitmap at exactly `key`, marked drawn.
    pub fn get(&mut self, key: &PageKey) -> Option<PenikoImage> {
        let frame = self.frame;
        self.held
            .get_mut(key)
            .filter(|held| !held.stale)
            .map(|held| {
                held.used = frame;
                held.image.clone()
            })
    }

    /// The bitmap to paint `page` with when it wants `bucket`: that one if it
    /// is here, else the smallest wider one, else the widest narrower one.
    /// Marked drawn. Stale bitmaps are not considered ([`Self::stale`]).
    pub fn best(&mut self, page: usize, bucket: u32) -> Option<(u32, PenikoImage)> {
        let key = self
            .held
            .iter()
            .filter(|((p, _), held)| *p == page && !held.stale)
            .map(|((_, b), _)| *b)
            .min_by_key(|b| {
                if *b >= bucket {
                    (0, b.saturating_sub(bucket))
                } else {
                    (1, bucket.saturating_sub(*b))
                }
            })?;
        self.get(&(page, key)).map(|image| (key, image))
    }

    /// The widest bitmap of an earlier version of `page`, to draw until its
    /// current one arrives. Marked drawn.
    pub fn stale(&mut self, page: usize) -> Option<PenikoImage> {
        let frame = self.frame;
        self.held
            .iter_mut()
            .filter(|((p, _), held)| *p == page && held.stale)
            .max_by_key(|((_, b), _)| *b)
            .map(|(_, held)| {
                held.used = frame;
                held.image.clone()
            })
    }

    /// Hold `image` as the current bitmap at `key`. The page's stale
    /// stand-ins go: they are never drawn over a current bitmap.
    pub fn insert(&mut self, key: PageKey, image: PenikoImage) {
        let bytes = (image.width as usize)
            .saturating_mul(image.height as usize)
            .saturating_mul(4);
        self.remove(&key);
        let stand_ins: Vec<PageKey> = self
            .held
            .iter()
            .filter(|((p, _), held)| *p == key.0 && held.stale)
            .map(|(k, _)| *k)
            .collect();
        for stand_in in stand_ins {
            self.remove(&stand_in);
        }
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
                stale: false,
            },
        );
    }

    /// Follow a new version of the document: each held page's bitmaps go
    /// where `map` says, and those it maps nowhere are dropped. Two old pages
    /// carried to one index keep the fresh one's bitmaps.
    pub fn carry(&mut self, map: impl Fn(usize) -> Option<Carry>) {
        let old = std::mem::take(&mut self.held);
        self.bytes = 0;
        let mut carried: HashMap<PageKey, Held> = HashMap::with_capacity(old.len());
        for ((page, bucket), mut held) in old {
            let (to, stale) = match map(page) {
                Some(Carry::Fresh(to)) => (to, held.stale),
                Some(Carry::Stale(to)) => (to, true),
                None => continue,
            };
            held.stale = stale;
            match carried.get(&(to, bucket)) {
                Some(there) if !there.stale || stale => {}
                _ => {
                    carried.insert((to, bucket), held);
                }
            }
        }
        self.bytes = carried
            .values()
            .fold(0, |bytes, held| bytes.saturating_add(held.bytes));
        self.held = carried;
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
        PenikoImage {
            data: vec![0u8; usize::try_from(width.pow(2).saturating_mul(4)).unwrap_or(0)].into(),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width,
            height: width,
        }
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
        images.begin_frame();
        images.insert((4, 16), image(16));
        assert!(images.bytes() <= 2 * one);
    }

    #[test]
    fn a_new_document_version_keeps_unchanged_pages_and_stand_ins_for_changed_ones() {
        let mut images = PageImages::new(usize::MAX);
        images.begin_frame();
        for page in 0..4 {
            images.insert((page, 48), image(48));
        }
        // A page was inserted before page 2: pages 2 and 3 moved down
        // unchanged, page 1 changed, page 0 is as it was.
        images.carry(|page| match page {
            0 => Some(Carry::Fresh(0)),
            1 => Some(Carry::Stale(1)),
            2 => Some(Carry::Fresh(3)),
            3 => Some(Carry::Fresh(4)),
            _ => None,
        });
        assert!(images.contains_key(&(0, 48)));
        assert!(
            !images.contains_key(&(1, 48)),
            "a changed page is not current"
        );
        assert!(images.best(1, 48).is_none());
        assert!(images.stale(1).is_some(), "but stands in until it renders");
        assert!(images.contains_key(&(3, 48)) && images.contains_key(&(4, 48)));
        assert!(images.stale(2).is_none() && images.best(2, 48).is_none());
        assert_eq!(images.bytes(), 4 * 48 * 48 * 4);

        images.insert((1, 96), image(96));
        assert!(images.stale(1).is_none(), "the current bitmap replaces it");
        assert_eq!(images.bytes(), 3 * 48 * 48 * 4 + 96 * 96 * 4);
    }
}
