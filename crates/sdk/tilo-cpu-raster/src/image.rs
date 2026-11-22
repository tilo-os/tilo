//! Scene images converted to `vello_cpu` pixmaps, kept while they are drawn.

use std::collections::HashMap;
use std::sync::Arc;

use vello_cpu::peniko::{ImageAlphaType, ImageData, ImageFormat};
use vello_cpu::{PixelMetadata, Pixmap};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    blob: u64,
    width: u32,
    height: u32,
    bgra: bool,
    premultiplied: bool,
}

#[derive(Default)]
pub(crate) struct ImageCache {
    entries: HashMap<Key, (Arc<Pixmap>, u64)>,
    /// The pixmaps held, by a hash of what they hold: an image handed over again in a blob of its
    /// own (a view that makes its picture anew each frame) is the pixmap already here, so a scene
    /// that draws it is the same draw as before and the picture is not drawn again for it.
    by_pixels: HashMap<u64, Vec<Arc<Pixmap>>>,
    frame: u64,
}

/// Frames an image may go undrawn before its pixmap is dropped.
const MAX_IDLE_FRAMES: u64 = 2;

impl ImageCache {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries
            .retain(|_, (_, last)| frame - *last <= MAX_IDLE_FRAMES);
        // A pixmap no blob names any more, and no scene draws, goes with them.
        self.by_pixels.retain(|_, held| {
            held.retain(|pixmap| Arc::strong_count(pixmap) > 1);
            !held.is_empty()
        });
    }

    /// The premultiplied RGBA8 pixmap for `image`; `None` when it is empty,
    /// too large for `vello_cpu`, or its data is shorter than its size says.
    pub fn get(&mut self, image: &ImageData) -> Option<Arc<Pixmap>> {
        let key = Key {
            blob: image.data.id(),
            width: image.width,
            height: image.height,
            bgra: image.format == ImageFormat::Bgra8,
            premultiplied: image.alpha_type == ImageAlphaType::AlphaPremultiplied,
        };
        let frame = self.frame;
        if let Some((pixmap, last)) = self.entries.get_mut(&key) {
            *last = frame;
            return Some(pixmap.clone());
        }
        let width = u16::try_from(image.width).ok()?;
        let height = u16::try_from(image.height).ok()?;
        let len = usize::from(width) * usize::from(height) * 4;
        if len == 0 {
            return None;
        }
        let mut data = image.data.data().get(..len)?.to_vec();
        if key.bgra {
            for px in data.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        // Knowing an image is opaque lets a move of it be a move of pixels;
        // one pass when the image is first seen.
        let opaque = data.chunks_exact(4).all(|px| px[3] == u8::MAX);
        let pixmap = Pixmap::from_parts(
            data,
            width,
            height,
            PixelMetadata::new(image.alpha_type, !opaque),
        );
        let held = self.by_pixels.entry(content_hash(&pixmap)).or_default();
        let pixmap = if let Some(other) = held.iter().find(|other| same_pixels(other, &pixmap)) {
            Arc::clone(other)
        } else {
            let pixmap = Arc::new(pixmap);
            held.push(Arc::clone(&pixmap));
            pixmap
        };
        self.entries.insert(key, (pixmap.clone(), frame));
        Some(pixmap)
    }
}

fn content_hash(pixmap: &Pixmap) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (pixmap.width(), pixmap.height()).hash(&mut hasher);
    pixmap.data_as_u8_slice().hash(&mut hasher);
    hasher.finish()
}

fn same_pixels(a: &Pixmap, b: &Pixmap) -> bool {
    a.width() == b.width()
        && a.height() == b.height()
        && a.may_have_transparency() == b.may_have_transparency()
        && a.data_as_u8_slice() == b.data_as_u8_slice()
}
