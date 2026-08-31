//! Background image decode + cache for image elements.
//!
//! Image elements store only a filesystem path in the CRDT (see
//! [`crate::model::Element::src`]); the decoded pixels are a per-peer runtime
//! cache keyed by that path. Decoding runs on a worker thread so a large JPEG
//! never blocks the UI thread — the slide paints a light "loading" state until
//! the pixels land, then the real picture appears on the next frame.
//!
//! The decoded bytes are wrapped in a [`vello::peniko::ImageData`] (CPU-side
//! RGBA, no GPU handle) so the renderer can paint them directly with an
//! `ImageBrush`, exactly like [`ui::widget::Image`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// Current decode state of one image path.
#[derive(Clone)]
pub enum ImageState {
    /// A worker thread is decoding the file.
    Loading,
    /// Decoded, ready to paint.
    Ready(ImageData),
    /// The file was missing or the bytes weren't a decodable image.
    Failed,
}

/// A path-keyed image cache with off-thread decoding.
pub struct ImageStore {
    inner: RefCell<HashMap<String, ImageState>>,
    tx: Sender<(String, ImageState)>,
    rx: Receiver<(String, ImageState)>,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageStore {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        ImageStore {
            inner: RefCell::new(HashMap::new()),
            tx,
            rx,
        }
    }

    /// Look up the current state of `path`, kicking off a background decode the
    /// first time a path is seen. Cheap on every subsequent call (a map read +
    /// an `Arc` bump when ready).
    pub fn get(&self, path: &str) -> ImageState {
        if path.is_empty() {
            return ImageState::Failed;
        }
        let mut map = self.inner.borrow_mut();
        if let Some(state) = map.get(path) {
            return state.clone();
        }
        map.insert(path.to_string(), ImageState::Loading);
        let tx = self.tx.clone();
        let p = path.to_string();
        // One worker per newly-seen image. Inserts are rare (a user picking a
        // file), so a thread-per-decode is simpler than a pool and never
        // starves the UI thread.
        std::thread::Builder::new()
            .name("slides-image-decode".into())
            .spawn(move || {
                let state = decode(&p);
                let _ = tx.send((p, state));
            })
            .ok();
        ImageState::Loading
    }

    /// Decode `path` synchronously and cache the result. Used by the headless
    /// `--shots` path, where the single offscreen frame must have the pixels
    /// ready before it renders. Never called on the live UI thread.
    pub fn load_blocking(&self, path: &str) {
        let state = decode(path);
        self.inner.borrow_mut().insert(path.to_string(), state);
    }

    /// Drain finished decodes into the cache. Returns `true` if any image
    /// changed state (so the caller can invalidate render caches / request a
    /// frame). Called once per frame.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok((path, state)) = self.rx.try_recv() {
            self.inner.get_mut().insert(path, state);
            changed = true;
        }
        changed
    }

    /// Whether any tracked image is still decoding (the app keeps requesting
    /// frames until every decode settles, then rests).
    pub fn any_loading(&self) -> bool {
        self.inner
            .borrow()
            .values()
            .any(|s| matches!(s, ImageState::Loading))
    }
}

/// Decode an image file into straight-alpha RGBA8 wrapped for Vello. Returns
/// [`ImageState::Failed`] on any read/decode error.
fn decode(path: &str) -> ImageState {
    let Ok(bytes) = std::fs::read(path) else {
        return ImageState::Failed;
    };
    let Ok(img) = image::load_from_memory(&bytes) else {
        return ImageState::Failed;
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return ImageState::Failed;
    }
    let blob = Blob::new(std::sync::Arc::new(rgba.into_raw()));
    ImageState::Ready(ImageData {
        data: blob,
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: w,
        height: h,
    })
}
