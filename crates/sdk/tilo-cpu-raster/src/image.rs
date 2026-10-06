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
        let pixmap = Arc::new(Pixmap::from_parts(
            data,
            width,
            height,
            PixelMetadata::new(image.alpha_type, true),
        ));
        self.entries.insert(key, (pixmap.clone(), frame));
        Some(pixmap)
    }
}
