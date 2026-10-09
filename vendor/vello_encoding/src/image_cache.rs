// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use guillotiere::{AllocId, AtlasAllocator, size2};
use peniko::ImageData;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

const DEFAULT_ATLAS_SIZE: i32 = 1024;
const MAX_ATLAS_SIZE: i32 = 8192;
const EVICT_AFTER_GENERATIONS: u64 = 2;
/// Consecutive resolves the atlas must stay mostly empty before it halves.
/// Long enough that one frame without a window (a close animation, a
/// workspace switch) does not throw the atlas away only to grow it back.
const SHRINK_AFTER_RESOLVES: u32 = 60;
/// The atlas halves only while its residents cover at most `1 / N` of it,
/// so the halved atlas is at most half full and the next frame's
/// replacement images still fit without growing straight back.
const SHRINK_WHEN_AREA_AT_MOST_ONE_IN: u64 = 8;

/// What the image cache holds, for memory diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCacheStats {
    /// Side length of the square atlas, in pixels.
    pub atlas_side: u32,
    /// Images resident in the atlas.
    pub resident: usize,
    /// Bytes of CPU pixels the residents keep alive (their blobs). A blob
    /// the caller still holds is shared, not copied, so this is an upper
    /// bound on what the cache alone keeps.
    pub resident_cpu_bytes: u64,
    /// Area of the residents, in atlas pixels.
    pub resident_area: u64,
    /// Residents evicted over the cache's lifetime.
    pub evicted_total: u64,
    /// Times the atlas halved because its residents no longer needed it.
    pub shrinks_total: u64,
    /// Images omitted over the cache's lifetime.
    pub omitted_total: u64,
}

#[derive(Default)]
pub struct Images<'a> {
    /// Width of the square image atlas texture.
    pub width: u32,
    /// Height of the square image atlas texture.
    pub height: u32,
    /// Number of resident images evicted during the current resolve pass.
    ///
    /// This is only used for renderer-side debug logging.
    pub evicted: usize,
    /// Images omitted because they could not be allocated this resolve pass.
    pub omitted: usize,
    /// Images omitted over the lifetime of this cache.
    pub omitted_total: u64,
    /// Images that must be uploaded in the current resolve pass, with atlas locations.
    pub images: &'a [(ImageData, u32, u32)],
}

#[derive(Clone)]
struct ResidentImage {
    image: ImageData,
    alloc_id: AllocId,
    x: u32,
    y: u32,
    dirty: bool,
    last_used_generation: u64,
}

pub(crate) struct ImageCache {
    atlas: AtlasAllocator,
    /// Side length the atlas starts at, and never shrinks below.
    min_size: i32,
    /// Maximum side length for the square image atlas texture.
    max_size: i32,
    /// Resolves in a row whose residents fit in a fraction of the atlas; see
    /// [`SHRINK_AFTER_RESOLVES`].
    sparse_resolves: u32,
    /// Residents evicted over the cache's lifetime.
    evicted_total: u64,
    /// Times the atlas halved.
    shrinks_total: u64,
    /// Monotonic counter for resolve passes, used to track when resident images were last used.
    generation: u64,
    /// Number of resident images evicted during the current resolve pass.
    ///
    /// This is exposed through [`Images::evicted`] for renderer-side debug logging, and also
    /// prevents repeated stale-eviction scans during the same resolve pass.
    evicted_in_resolve: usize,
    /// Images omitted during the current resolve pass.
    omitted_in_resolve: usize,
    /// Images omitted over the lifetime of this cache.
    omitted_total: u64,
    /// Map from image blob id to atlas residency.
    map: HashMap<u64, ResidentImage>,
    /// Images that must be uploaded in the current resolve pass, with atlas locations.
    images: Vec<(ImageData, u32, u32)>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageCache {
    pub(crate) fn new() -> Self {
        Self::new_with_sizes(DEFAULT_ATLAS_SIZE, MAX_ATLAS_SIZE)
    }

    fn new_with_sizes(initial_size: i32, max_size: i32) -> Self {
        Self {
            atlas: AtlasAllocator::new(size2(initial_size, initial_size)),
            min_size: initial_size,
            max_size,
            sparse_resolves: 0,
            evicted_total: 0,
            shrinks_total: 0,
            generation: 0,
            evicted_in_resolve: 0,
            omitted_in_resolve: 0,
            omitted_total: 0,
            map: HashMap::default(),
            images: Vec::default(),
        }
    }

    /// Limit atlas growth to `max_size` pixels per side. A smaller limit only
    /// stops further growth; the atlas comes down on its own once its
    /// residents fit in a fraction of it (see [`Self::finish_resolve`]).
    pub(crate) fn set_max_size(&mut self, max_size: i32) {
        self.max_size = max_size.max(DEFAULT_ATLAS_SIZE);
    }

    /// What the cache holds now.
    pub(crate) fn stats(&self) -> ImageCacheStats {
        ImageCacheStats {
            atlas_side: self.atlas.size().width as u32,
            resident: self.map.len(),
            resident_cpu_bytes: self
                .map
                .values()
                .map(|resident| resident.image.data.len() as u64)
                .sum(),
            resident_area: self.resident_area(),
            evicted_total: self.evicted_total,
            shrinks_total: self.shrinks_total,
            omitted_total: self.omitted_total,
        }
    }

    pub(crate) fn begin_resolve(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.evicted_in_resolve = 0;
        self.omitted_in_resolve = 0;
        self.images.clear();
        // Halve here, before this pass looks anything up: every position this
        // pass hands out is then one in the halved atlas. Halving in
        // `finish_resolve` would move images the pass just encoded.
        if self.sparse_resolves >= SHRINK_AFTER_RESOLVES {
            self.sparse_resolves = 0;
            let half = self.atlas.size().width / 2;
            if half >= self.min_size && self.repack_to_size(half) {
                self.shrinks_total = self.shrinks_total.saturating_add(1);
            }
        }
    }

    pub(crate) fn restart_resolve_pass(&mut self) {
        self.images.clear();
        let previous_generation = self.generation.wrapping_sub(1);
        for resident in self.map.values_mut() {
            if resident.last_used_generation == self.generation {
                resident.last_used_generation = previous_generation;
            }
        }
    }

    pub(crate) fn images(&self) -> Images<'_> {
        Images {
            width: self.atlas.size().width as u32,
            height: self.atlas.size().height as u32,
            evicted: self.evicted_in_resolve,
            omitted: self.omitted_in_resolve,
            omitted_total: self.omitted_total,
            images: &self.images,
        }
    }

    pub(crate) fn bump_size(&mut self) -> bool {
        let mut new_size = self.atlas.size().width * 2;
        while new_size <= self.max_size {
            if self.repack_to_size(new_size) {
                self.images.clear();
                return true;
            }
            new_size *= 2;
        }
        false
    }

    pub(crate) fn get_or_insert(&mut self, image: &ImageData) -> Option<(u32, u32)> {
        match self.map.entry(image.data.id()) {
            Entry::Occupied(mut occupied) => {
                let resident = occupied.get_mut();
                let xy = (resident.x, resident.y);
                if resident.last_used_generation != self.generation {
                    resident.last_used_generation = self.generation;
                    if resident.dirty {
                        self.images
                            .push((resident.image.clone(), resident.x, resident.y));
                    }
                }
                Some(xy)
            }
            Entry::Vacant(vacant) => {
                let alloc = self
                    .atlas
                    .allocate(size2(image.width as _, image.height as _))?;
                let x = alloc.rectangle.min.x as u32;
                let y = alloc.rectangle.min.y as u32;
                let resident = ResidentImage {
                    image: image.clone(),
                    alloc_id: alloc.id,
                    x,
                    y,
                    dirty: true,
                    last_used_generation: self.generation,
                };
                self.images.push((image.clone(), x, y));
                vacant.insert(resident);
                Some((x, y))
            }
        }
    }

    /// End a resolve pass: release what the last few passes stopped drawing,
    /// and note whether the atlas is far bigger than its residents need.
    ///
    /// Eviction used to wait for an allocation to fail. A client frame is a
    /// new image every commit, so every superseded frame kept its CPU pixels
    /// alive until the atlas was full — and the atlas grew to its maximum
    /// first, which it then kept for the life of the process.
    pub(crate) fn finish_resolve(&mut self) {
        for resident in self.map.values_mut() {
            if resident.last_used_generation == self.generation {
                resident.dirty = false;
            }
        }
        // This pass's high-water mark, before the stale entries go: the
        // atlas had to hold all of it.
        let peak_area = self.resident_area();
        self.evict_unused();
        let side = self.atlas.size().width;
        let atlas_area = (side as u64) * (side as u64);
        if side > self.min_size
            && peak_area.saturating_mul(SHRINK_WHEN_AREA_AT_MOST_ONE_IN) <= atlas_area
        {
            self.sparse_resolves = self.sparse_resolves.saturating_add(1);
        } else {
            self.sparse_resolves = 0;
        }
    }

    fn resident_area(&self) -> u64 {
        self.map
            .values()
            .map(|resident| u64::from(resident.image.width) * u64::from(resident.image.height))
            .sum()
    }

    /// Evict every resident no pass drew in the last
    /// [`EVICT_AFTER_GENERATIONS`] passes. Returns how many went.
    fn evict_unused(&mut self) -> usize {
        let Some(stale_before) = self.generation.checked_sub(EVICT_AFTER_GENERATIONS) else {
            return 0;
        };
        let mut evicted = 0;
        for (_id, resident) in self
            .map
            .extract_if(|_, resident| resident.last_used_generation < stale_before)
        {
            self.atlas.deallocate(resident.alloc_id);
            evicted += 1;
        }
        self.evicted_in_resolve += evicted;
        self.evicted_total = self.evicted_total.saturating_add(evicted as u64);
        evicted
    }

    pub(crate) fn mark_dirty(&mut self, image: &ImageData) {
        if let Some(resident) = self.map.get_mut(&image.data.id()) {
            resident.dirty = true;
        }
    }

    pub(crate) fn can_fit_image(&self, image: &ImageData) -> bool {
        image.width <= self.atlas.size().width as u32
            && image.height <= self.atlas.size().height as u32
    }

    pub(crate) fn record_omission(&mut self) {
        self.omitted_in_resolve = self.omitted_in_resolve.saturating_add(1);
        self.omitted_total = self.omitted_total.saturating_add(1);
    }

    pub(crate) fn evict_stale_entries(&mut self) -> bool {
        if self.evicted_in_resolve != 0 {
            return false;
        }
        self.evict_unused() != 0
    }

    fn repack_to_size(&mut self, size: i32) -> bool {
        let mut atlas = AtlasAllocator::new(size2(size, size));
        let mut entries: Vec<_> = self.map.iter().collect();
        entries.sort_by_key(|(id, _)| *id);
        let mut map = HashMap::with_capacity(self.map.len());
        for (id, resident) in entries {
            let Some(alloc) =
                atlas.allocate(size2(resident.image.width as _, resident.image.height as _))
            else {
                return false;
            };
            let mut resident = resident.clone();
            resident.alloc_id = alloc.id;
            resident.x = alloc.rectangle.min.x as u32;
            resident.y = alloc.rectangle.min.y as u32;
            resident.dirty = true;
            map.insert(*id, resident);
        }
        self.atlas = atlas;
        self.map = map;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peniko::{Blob, ImageAlphaType, ImageFormat};
    use std::sync::Arc;

    fn image(id_byte: u8, width: u32, height: u32) -> ImageData {
        let len = (width * height * 4) as usize;
        ImageData {
            data: Blob::new(Arc::new(vec![id_byte; len])),
            format: ImageFormat::Rgba8,
            width,
            height,
            alpha_type: ImageAlphaType::Alpha,
        }
    }

    #[test]
    fn atlas_size_persists_after_growth() {
        let mut cache = ImageCache::new_with_sizes(16, 64);
        assert_eq!(cache.atlas.size().width, 16);
        assert!(cache.bump_size());
        assert_eq!(cache.atlas.size().width, 32);
        cache.begin_resolve();
        assert_eq!(cache.atlas.size().width, 32);
    }

    #[test]
    fn a_lowered_maximum_stops_atlas_growth() {
        let mut cache = ImageCache::new();
        cache.set_max_size(2048);
        assert!(cache.bump_size());
        assert_eq!(cache.atlas.size().width, 2048);
        assert!(!cache.bump_size(), "grew past the 2048 limit");
        assert_eq!(cache.atlas.size().width, 2048);
    }

    #[test]
    fn omission_count_is_per_pass_and_cumulative() {
        let mut cache = ImageCache::new_with_sizes(16, 16);
        cache.begin_resolve();
        cache.record_omission();
        cache.record_omission();
        assert_eq!(cache.images().omitted, 2);
        assert_eq!(cache.images().omitted_total, 2);

        cache.begin_resolve();
        assert_eq!(cache.images().omitted, 0);
        assert_eq!(cache.images().omitted_total, 2);
    }

    #[test]
    fn resident_entries_are_reused_across_resolves() {
        let mut cache = ImageCache::new_with_sizes(32, 64);
        let image = image(7, 8, 8);

        cache.begin_resolve();
        let first = cache.get_or_insert(&image).unwrap();
        assert_eq!(cache.images.len(), 1);
        cache.finish_resolve();

        cache.begin_resolve();
        let second = cache.get_or_insert(&image).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.images.len(), 0);
        assert_eq!(cache.map.len(), 1);
    }

    #[test]
    fn marked_dirty_resident_entries_are_uploaded_again() {
        let mut cache = ImageCache::new_with_sizes(32, 64);
        let image = image(7, 8, 8);

        cache.begin_resolve();
        let first = cache.get_or_insert(&image).unwrap();
        cache.finish_resolve();

        cache.mark_dirty(&image);
        cache.begin_resolve();
        let second = cache.get_or_insert(&image).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.images.len(), 1);
        assert_eq!(cache.images[0].0.data.id(), image.data.id());
        assert_eq!((cache.images[0].1, cache.images[0].2), first);
        cache.finish_resolve();

        cache.begin_resolve();
        assert_eq!(cache.get_or_insert(&image), Some(first));
        assert_eq!(cache.images.len(), 0);
    }

    #[test]
    fn marked_dirty_unused_entries_stay_dirty() {
        let mut cache = ImageCache::new_with_sizes(32, 64);
        let image = image(7, 8, 8);

        cache.begin_resolve();
        let xy = cache.get_or_insert(&image).unwrap();
        cache.finish_resolve();

        cache.mark_dirty(&image);
        cache.begin_resolve();
        cache.finish_resolve();

        cache.begin_resolve();
        assert_eq!(cache.get_or_insert(&image), Some(xy));
        assert_eq!(cache.images.len(), 1);
    }

    #[test]
    fn stale_entries_can_be_evicted_under_pressure() {
        let mut cache = ImageCache::new_with_sizes(16, 16);
        let image_a = image(1, 10, 10);
        let image_b = image(2, 10, 10);

        cache.begin_resolve();
        assert!(cache.get_or_insert(&image_a).is_some());

        cache.begin_resolve();

        cache.begin_resolve();
        cache.begin_resolve();
        assert!(cache.get_or_insert(&image_b).is_none());
        assert!(cache.evict_stale_entries());
        assert!(cache.get_or_insert(&image_b).is_some());
        assert!(!cache.map.contains_key(&image_a.data.id()));
    }

    #[test]
    fn stale_entries_are_evicted_at_most_once_per_resolve() {
        let mut cache = ImageCache::new_with_sizes(32, 32);
        let image_a = image(1, 8, 8);
        let image_b = image(2, 8, 8);

        cache.begin_resolve();
        assert!(cache.get_or_insert(&image_a).is_some());
        cache.finish_resolve();

        cache.begin_resolve();
        assert!(cache.get_or_insert(&image_b).is_some());
        cache.finish_resolve();

        cache.begin_resolve();
        cache.begin_resolve();
        cache.begin_resolve();

        assert!(cache.evict_stale_entries());
        assert!(!cache.evict_stale_entries());
    }

    /// One resolve pass drawing `images`, as `Resolver::resolve` and its
    /// `resolve_pending_images` run it: evict under pressure, else grow and
    /// start the pass over.
    fn pass(cache: &mut ImageCache, images: &[&ImageData]) -> Vec<(u32, u32)> {
        cache.begin_resolve();
        let at = 'outer: loop {
            cache.restart_resolve_pass();
            let mut at = Vec::new();
            for image in images {
                let xy = loop {
                    if let Some(xy) = cache.get_or_insert(image) {
                        break xy;
                    }
                    if cache.can_fit_image(image) && cache.evict_stale_entries() {
                        continue;
                    }
                    assert!(
                        cache.bump_size(),
                        "no room for a {}x{}",
                        image.width,
                        image.height
                    );
                    continue 'outer;
                };
                at.push(xy);
            }
            break at;
        };
        cache.finish_resolve();
        at
    }

    #[test]
    fn stale_residents_are_released_without_allocation_pressure() {
        let mut cache = ImageCache::new_with_sizes(64, 64);
        let image_a = image(1, 8, 8);
        let image_b = image(2, 8, 8);
        let weak_a = image_a.data.downgrade();

        pass(&mut cache, &[&image_a]);
        drop(image_a);
        assert!(
            weak_a.upgrade().is_some(),
            "a just-drawn image stays resident"
        );

        for _ in 0..3 {
            pass(&mut cache, &[&image_b]);
        }
        assert!(
            weak_a.upgrade().is_none(),
            "the cache let go of A's pixels with the atlas nowhere near full"
        );
        let stats = cache.stats();
        assert_eq!(stats.resident, 1);
        assert_eq!(stats.resident_cpu_bytes, image_b.data.len() as u64);
        assert_eq!(stats.evicted_total, 1);
    }

    #[test]
    fn an_image_survives_two_passes_that_do_not_draw_it() {
        // Two scenes sharing one renderer (two outputs, a capture beside the
        // live frame) each skip the other's images; that must not re-upload.
        let mut cache = ImageCache::new_with_sizes(64, 64);
        let image_a = image(1, 8, 8);
        let image_b = image(2, 8, 8);
        let image_c = image(3, 8, 8);
        for _ in 0..10 {
            pass(&mut cache, &[&image_a]);
            pass(&mut cache, &[&image_b]);
            pass(&mut cache, &[&image_c]);
        }
        assert_eq!(cache.stats().evicted_total, 0);
        assert_eq!(cache.stats().resident, 3);
    }

    #[test]
    fn a_window_that_keeps_committing_holds_three_frames_at_most() {
        let mut cache = ImageCache::new_with_sizes(256, 1024);
        let frame_bytes = (100 * 80 * 4) as u64;
        for commit in 0..200_u32 {
            let frame = image(u8::try_from(commit % 251).unwrap(), 100, 80);
            pass(&mut cache, &[&frame]);
            assert!(
                cache.stats().resident_cpu_bytes <= 3 * frame_bytes,
                "commit {commit}: {:?}",
                cache.stats()
            );
        }
        assert_eq!(cache.stats().atlas_side, 256, "never grew");
    }

    #[test]
    fn atlas_shrinks_after_residency_drops() {
        let mut cache = ImageCache::new_with_sizes(1024, 4096);
        let big: Vec<_> = (0..4).map(|i| image(i, 1500, 1500)).collect();
        let small = image(9, 256, 256);
        pass(&mut cache, &big.iter().collect::<Vec<_>>());
        assert_eq!(cache.atlas.size().width, 4096);
        drop(big);

        for _ in 0..(3 * (SHRINK_AFTER_RESOLVES + 1)) {
            pass(&mut cache, &[&small]);
        }
        let stats = cache.stats();
        assert_eq!(stats.atlas_side, 1024, "{stats:?}");
        assert_eq!(stats.shrinks_total, 2);
        assert_eq!(stats.resident, 1);
    }

    #[test]
    fn a_shrunk_atlas_uploads_its_residents_again_where_it_draws_them() {
        let mut cache = ImageCache::new_with_sizes(32, 64);
        let large = image(1, 40, 40);
        let small = image(2, 8, 8);
        pass(&mut cache, &[&large, &small]);
        assert_eq!(cache.atlas.size().width, 64);
        drop(large);

        let mut uploads = Vec::new();
        for _ in 0..(SHRINK_AFTER_RESOLVES + 4) {
            cache.begin_resolve();
            let xy = cache.get_or_insert(&small).expect("fits");
            uploads.extend(
                cache
                    .images
                    .iter()
                    .map(|(image, x, y)| (image.data.id(), (*x, *y), xy)),
            );
            cache.finish_resolve();
        }
        assert_eq!(cache.stats().atlas_side, 32);
        // Uploaded on first use, and once more after the shrink moved it.
        let after_shrink = uploads.last().expect("re-uploaded");
        assert_eq!(after_shrink.0, small.data.id());
        assert_eq!(after_shrink.1, after_shrink.2, "uploaded where it is drawn");
        assert_eq!(cache.stats().shrinks_total, 1);
    }

    #[test]
    fn a_busy_atlas_does_not_shrink() {
        let mut cache = ImageCache::new_with_sizes(32, 64);
        let large = image(1, 40, 40);
        for _ in 0..(4 * SHRINK_AFTER_RESOLVES) {
            pass(&mut cache, &[&large]);
        }
        assert_eq!(cache.stats().atlas_side, 64);
        assert_eq!(cache.stats().shrinks_total, 0);
    }

    /// The compositor's open/close cycle: a wallpaper drawn every frame, a
    /// window that commits a new frame per paint while open, then closes.
    /// What the cache holds after each close must not grow with the cycles.
    #[test]
    fn open_close_cycles_leave_nothing_behind() {
        let mut cache = ImageCache::new_with_sizes(1024, 8192);
        let wallpaper = image(0, 1920, 1080);
        let mut after_close = Vec::new();
        for cycle in 0..30_u32 {
            let (width, height) = (800 + 40 * (cycle % 5), 600 + 30 * (cycle % 3));
            for commit in 0..12_u32 {
                let frame = image(
                    u8::try_from((cycle * 12 + commit) % 251).unwrap(),
                    width,
                    height,
                );
                pass(&mut cache, &[&wallpaper, &frame]);
            }
            for _ in 0..(SHRINK_AFTER_RESOLVES + 4) {
                pass(&mut cache, &[&wallpaper]);
            }
            after_close.push(cache.stats());
        }
        let first = after_close[0];
        for (cycle, stats) in after_close.iter().enumerate() {
            assert_eq!(stats.resident, 1, "cycle {cycle}: {stats:?}");
            assert_eq!(
                stats.resident_cpu_bytes,
                wallpaper.data.len() as u64,
                "cycle {cycle}: {stats:?}"
            );
            assert_eq!(
                stats.atlas_side, first.atlas_side,
                "cycle {cycle}: {stats:?}"
            );
        }
        assert!(first.atlas_side <= 2048, "{first:?}");
    }

    #[test]
    fn failed_repack_leaves_existing_residency_unchanged() {
        let mut cache = ImageCache::new_with_sizes(16, 16);
        let image = image(1, 12, 12);

        cache.begin_resolve();
        let xy = cache.get_or_insert(&image).unwrap();

        assert!(!cache.repack_to_size(8));
        assert_eq!(cache.atlas.size().width, 16);
        assert_eq!(cache.get_or_insert(&image), Some(xy));
        assert_eq!(cache.map.len(), 1);
    }
}
