//! Background image decode + cache for image elements.
//!
//! Image elements store only a content address in the CRDT (see
//! [`crate::model::Element::src`]); the bytes live in the app's
//! [`ContentStore`] and decoded pixels are a per-peer runtime cache keyed by
//! that identity. Decoding runs on a worker thread so a large
//! JPEG never blocks the UI thread — the slide paints a light "loading" state
//! until the pixels land, then the real picture appears on the next frame.
//!
//! The decoded bytes are wrapped in a [`vello::peniko::ImageData`] (CPU-side
//! RGBA, no GPU handle) so the renderer can paint them directly with an
//! `ImageBrush`, exactly like [`ui::widget::Image`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use tilo_content::ContentStore;
use vello::peniko::{Blob, ImageData};

/// A slide may refer to a very large source photo, but the editor must never
/// turn that reference into an unbounded read or allocation.  These ceilings
/// are deliberately above ordinary camera pictures while remaining small
/// enough to keep one malformed image from taking the desktop down.
pub const MAX_IMAGE_FILE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_IMAGE_PIXELS: u64 = 24 * 1024 * 1024;
const MAX_IMAGE_DECODE_BYTES: u64 = MAX_IMAGE_PIXELS * 4;
const DECODE_QUEUE_CAPACITY: usize = 8;
const DECODE_RESULT_CAPACITY: usize = 1;
const MAX_READY_IMAGE_BYTES: usize = 96 * 1024 * 1024;
/// How long a picture whose bytes are still arriving from another device
/// waits before it is read again.
const ARRIVING_RETRY: std::time::Duration = std::time::Duration::from_millis(500);
pub const ASSET_PREFIX: &str = "asset:blake3:";

/// Where pictures are kept when no host lent the window a store (the
/// store a host lends carries them on the deck's route): in memory.
pub fn app_content() -> ContentStore {
    ContentStore::memory()
}

/// A deck carries this content identity, never a machine-local file path.
pub fn is_asset_id(id: &str) -> bool {
    id.strip_prefix(ASSET_PREFIX)
        .is_some_and(tilo_content::valid_hash)
}

/// Store supplied bytes in `content` by their BLAKE3 content hash and return
/// the portable identifier that is safe to place in the collaborative deck. A
/// later peer can request the bytes by this hash.
pub fn store_asset(content: &ContentStore, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_FILE_BYTES {
        return None;
    }
    // Do not create a CRDT reference unless the store accepted the bytes.
    match content.put(bytes) {
        Ok(hash) => Some(format!("{ASSET_PREFIX}{hash}")),
        Err(error) => {
            eprintln!("slides: content store refused an image: {error}");
            None
        }
    }
}

/// Resolve a deck asset only after checking both its bounded bytes and its
/// declared hash (the store verifies every read).  A missing or corrupt blob
/// is an ordinary unavailable asset, never an instruction to read an arbitrary
/// peer-provided filesystem path.
pub fn read_asset(content: &ContentStore, id: &str) -> Option<Vec<u8>> {
    if !is_asset_id(id) {
        return None;
    }
    let hash = id.strip_prefix(ASSET_PREFIX)?;
    let bytes = content.read(hash)?;
    (bytes.len() <= MAX_IMAGE_FILE_BYTES).then_some(bytes)
}

pub fn import_asset_file(content: &ContentStore, path: &Path) -> Option<String> {
    let bytes = read_bounded(path, MAX_IMAGE_FILE_BYTES).ok()??;
    store_asset(content, &bytes)
}

/// The file a chooser named, or `None` when it is larger than `limit`.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    let (dir, name) = tilo_files::file_at(path).map_err(std::io::Error::other)?;
    tilo_files::blocking::read_bounded(&dir, &name, limit).map_err(std::io::Error::other)
}

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

/// Read-only image lookup shared by live rendering and frozen history.
pub trait ImageLookup {
    fn get(&self, id: &str) -> ImageState;
}
impl ImageLookup for ImageStore {
    fn get(&self, id: &str) -> ImageState {
        ImageStore::get(self, id)
    }
}

/// A path-keyed image cache with off-thread decoding.
pub struct ImageStore {
    content: ContentStore,
    inner: RefCell<HashMap<String, ImageState>>,
    request_tx: Option<SyncSender<String>>,
    rx: Receiver<(String, ImageState)>,
    ready_bytes: Cell<usize>,
    access: RefCell<HashMap<String, u64>>,
    access_clock: Cell<u64>,
    /// Pictures shown as still arriving whatever their decode reports.
    held: RefCell<std::collections::HashSet<String>>,
    /// Pictures whose bytes another device is still sending this one, and
    /// when they were last asked for.
    arriving: RefCell<HashMap<String, ui::time::Instant>>,
}

impl ImageStore {
    /// A cache decoding the images `content` holds.
    pub fn new(content: ContentStore) -> Self {
        let (result_tx, rx) =
            std::sync::mpsc::sync_channel::<(String, ImageState)>(DECODE_RESULT_CAPACITY);
        let (request_tx, request_rx) =
            std::sync::mpsc::sync_channel::<String>(DECODE_QUEUE_CAPACITY);
        // One long-lived worker is both cheaper than a thread per image and
        // gives the cache an explicit, bounded back-pressure point.  A failed
        // spawn is represented by `None`; callers then receive `Failed`
        // rather than an eternal loading placeholder.
        let worker_content = content.clone();
        let request_tx = ui::rt::spawn_thread("slides-image-decode", move || {
            while let Ok(path) = request_rx.recv() {
                let state = decode(&worker_content, &path);
                if result_tx.send((path, state)).is_err() {
                    break;
                }
            }
        })
        .ok()
        .map(|_| request_tx);
        ImageStore {
            content,
            inner: RefCell::new(HashMap::new()),
            request_tx,
            rx,
            ready_bytes: Cell::new(0),
            access: RefCell::new(HashMap::new()),
            access_clock: Cell::new(0),
            held: RefCell::default(),
            arriving: RefCell::default(),
        }
    }

    /// Look up the current state of `path`, kicking off a background decode the
    /// first time a path is seen. Cheap on every subsequent call (a map read +
    /// an `Arc` bump when ready).
    pub fn get(&self, path: &str) -> ImageState {
        if !is_asset_id(path) {
            return ImageState::Failed;
        }
        let mut map = self.inner.borrow_mut();
        // Bytes arriving from another device are asked for again, now and
        // then, until they are here.
        let due = self
            .arriving
            .borrow()
            .get(path)
            .is_some_and(|asked| asked.elapsed() >= ARRIVING_RETRY);
        if due {
            self.arriving.borrow_mut().remove(path);
            map.remove(path);
        }
        if let Some(state) = map.get(path) {
            self.touch(path);
            return state.clone();
        }
        map.insert(path.to_string(), ImageState::Loading);
        let p = path.to_string();
        match self.request_tx.as_ref().map(|tx| tx.try_send(p)) {
            Some(Ok(())) => ImageState::Loading,
            // Queue saturation is not an accepted request: record a settled
            // failure, rather than displaying a spinner that no worker owns.
            Some(Err(TrySendError::Full(_) | TrySendError::Disconnected(_))) | None => {
                map.insert(path.to_string(), ImageState::Failed);
                ImageState::Failed
            }
        }
    }

    /// Decode `path` synchronously and cache the result: a screenshot, whose
    /// frame must have the pixels before it renders. Never called on the
    /// live UI thread.
    pub fn load_blocking(&self, path: &str) {
        let state = decode(&self.content, path);
        self.remove_ready_bytes(path);
        self.ready_bytes
            .set(self.ready_bytes.get().saturating_add(state_bytes(&state)));
        self.inner.borrow_mut().insert(path.to_string(), state);
        self.touch(path);
        self.evict_ready();
    }

    /// Show `path` as still loading from now on, whatever its decode
    /// reports: a screenshot of the frames before a picture's bytes arrive.
    pub fn hold_loading(&self, path: &str) {
        self.remove_ready_bytes(path);
        self.inner
            .borrow_mut()
            .insert(path.to_string(), ImageState::Loading);
        self.held.borrow_mut().insert(path.to_string());
    }

    /// Drain finished decodes into the cache. Returns `true` if any image
    /// changed state (so the caller can invalidate render caches / request a
    /// frame). Called once per frame.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok((path, state)) = self.rx.try_recv() {
            if self.held.borrow().contains(&path) {
                continue;
            }
            if matches!(state, ImageState::Loading) {
                self.arriving
                    .borrow_mut()
                    .insert(path.clone(), ui::time::now());
            }
            self.remove_ready_bytes(&path);
            self.ready_bytes
                .set(self.ready_bytes.get().saturating_add(state_bytes(&state)));
            self.inner.get_mut().insert(path.clone(), state);
            self.touch(&path);
            self.evict_ready();
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

impl ImageStore {
    fn touch(&self, path: &str) {
        let next = self.access_clock.get().checked_add(1).unwrap_or(1);
        self.access_clock.set(next);
        self.access.borrow_mut().insert(path.to_string(), next);
    }

    fn remove_ready_bytes(&self, path: &str) {
        if let Some(old) = self.inner.borrow().get(path) {
            self.ready_bytes
                .set(self.ready_bytes.get().saturating_sub(state_bytes(old)));
        }
    }

    fn evict_ready(&self) {
        while self.ready_bytes.get() > MAX_READY_IMAGE_BYTES {
            let candidate = {
                let map = self.inner.borrow();
                let access = self.access.borrow();
                map.iter()
                    .filter(|(_, state)| matches!(state, ImageState::Ready(_)))
                    .min_by_key(|(path, _)| access.get(*path).copied().unwrap_or(0))
                    .map(|(path, state)| (path.clone(), state_bytes(state)))
            };
            let Some((path, bytes)) = candidate else {
                break;
            };
            self.inner.borrow_mut().remove(&path);
            self.access.borrow_mut().remove(&path);
            self.ready_bytes
                .set(self.ready_bytes.get().saturating_sub(bytes));
        }
    }
}

fn state_bytes(state: &ImageState) -> usize {
    match state {
        ImageState::Ready(image) => {
            usize::try_from(u64::from(image.width) * u64::from(image.height) * 4)
                .unwrap_or(usize::MAX)
        }
        ImageState::Loading | ImageState::Failed => 0,
    }
}

/// Decode an image file into straight-alpha RGBA8 wrapped for Vello. Returns
/// [`ImageState::Failed`] on any read/decode error.
fn decode(content: &ContentStore, path: &str) -> ImageState {
    // This is deliberately categorical. The string arrived through a shared
    // CRDT and must never become a peer-controlled local-path probe. Screenshot
    // fixtures use `store_asset` first, exactly as the real portal does.
    if !is_asset_id(path) {
        return ImageState::Failed;
    }
    let Some(bytes) = read_asset(content, path) else {
        // Bytes another device holds, which this device's store is fetching
        // or will fetch once that device connects, are still on their way:
        // the picture is loading, not broken.
        let arriving = path.strip_prefix(ASSET_PREFIX).is_some_and(|hash| {
            matches!(
                content.state(hash),
                tilo_content::ContentState::Pending | tilo_content::ContentState::Waiting
            )
        });
        return if arriving {
            ImageState::Loading
        } else {
            ImageState::Failed
        };
    };
    decode_bytes(&bytes)
}

/// Pure bounded decode of already admitted bytes; no files, workers or stores.
pub(crate) fn decode_bytes(bytes: &[u8]) -> ImageState {
    decode_bytes_with_budget(bytes, MAX_READY_IMAGE_BYTES)
}

/// Reject dimensions before allocating pixels beyond a frozen preview's remainder.
pub(crate) fn decode_bytes_with_budget(bytes: &[u8], budget: usize) -> ImageState {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_FILE_BYTES {
        return ImageState::Failed;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    let max_decode_bytes = MAX_IMAGE_DECODE_BYTES.min(u64::try_from(budget).unwrap_or(u64::MAX));
    limits.max_alloc = Some(max_decode_bytes);
    let Ok(mut reader) =
        image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()
    else {
        return ImageState::Failed;
    };
    reader.limits(limits.clone());
    let Ok((w, h)) = reader.into_dimensions() else {
        return ImageState::Failed;
    };
    let pixels = u64::from(w).saturating_mul(u64::from(h));
    if w == 0 || h == 0 || pixels > MAX_IMAGE_PIXELS || pixels.saturating_mul(4) > max_decode_bytes
    {
        return ImageState::Failed;
    }
    let Ok(mut reader) =
        image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()
    else {
        return ImageState::Failed;
    };
    reader.limits(limits);
    let Ok(img) = reader.decode() else {
        return ImageState::Failed;
    };
    let rgba = img.to_rgba8();
    let (decoded_w, decoded_h) = rgba.dimensions();
    if decoded_w != w
        || decoded_h != h
        || rgba.len() > usize::try_from(MAX_IMAGE_DECODE_BYTES).unwrap_or(usize::MAX)
    {
        return ImageState::Failed;
    }
    let blob = Blob::new(std::sync::Arc::new(rgba.into_raw()));
    ImageState::Ready(ui::raster::rgba8(blob, decoded_w, decoded_h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_content::{
        ContentBackend, ContentError, ContentReader, ContentState, MemoryBackend, Stored,
    };

    /// A device store whose content arrives from another device: pending
    /// until `arrived`, then readable.
    struct Arriving {
        store: MemoryBackend,
        arrived: Arc<AtomicBool>,
    }

    impl ContentBackend for Arriving {
        fn put(&self, reader: &mut dyn std::io::Read, mime: &str) -> Result<Stored, ContentError> {
            self.store.put(reader, mime)
        }
        fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
            if self.arrived.load(Ordering::SeqCst) {
                self.store.open(hash)
            } else {
                Ok(None)
            }
        }
        fn has(&self, hash: &str) -> bool {
            self.arrived.load(Ordering::SeqCst) && self.store.has(hash)
        }
        fn state(&self, hash: &str) -> ContentState {
            if self.has(hash) {
                ContentState::Resolved
            } else {
                ContentState::Pending
            }
        }
    }

    /// A picture whose bytes are still on their way from another device
    /// shows as loading, and appears once they are here.
    #[test]
    fn a_picture_still_arriving_loads_and_then_appears() {
        let arrived = Arc::new(AtomicBool::new(false));
        let backend = Arriving {
            store: MemoryBackend::default(),
            arrived: Arc::clone(&arrived),
        };
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(3, 2));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let hash = backend
            .store
            .put(&mut bytes.get_ref().as_slice(), "image/png")
            .unwrap()
            .hash;
        let src = format!("{ASSET_PREFIX}{hash}");
        let mut images = ImageStore::new(ContentStore::from_backend(Arc::new(backend)));
        let settle = |images: &mut ImageStore| {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            loop {
                images.pump();
                let state = images.get(&src);
                if !images.arriving.borrow().is_empty() || !matches!(state, ImageState::Loading) {
                    return state;
                }
                assert!(std::time::Instant::now() < deadline, "the decode settles");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert!(
            matches!(settle(&mut images), ImageState::Loading),
            "bytes on their way are loading, not a broken picture"
        );
        arrived.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            images.pump();
            if let ImageState::Ready(ready) = images.get(&src) {
                assert_eq!((ready.width, ready.height), (3, 2));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the picture appears");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Frozen/live decode budgets and limits, and failed worker requests never stay loading.
    #[test]
    fn image_decoding_is_bounded_and_never_left_loading() {
        // Case `frozen_decode_refuses_pixels_exceeding_remaining_aggregate_budget`.
        {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            assert!(matches!(
                decode_bytes_with_budget(bytes.get_ref(), 15),
                ImageState::Failed
            ));
            assert!(matches!(
                decode_bytes_with_budget(bytes.get_ref(), 16),
                ImageState::Ready(_)
            ));
        }

        // Case `frozen_decode_keeps_file_dimension_and_corrupt_byte_limits`.
        {
            assert!(matches!(
                decode_bytes(&vec![0; MAX_IMAGE_FILE_BYTES + 1]),
                ImageState::Failed
            ));
            assert!(matches!(decode_bytes(b"corrupt"), ImageState::Failed));
            let image =
                image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, MAX_IMAGE_DIMENSION + 1));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            assert!(matches!(decode_bytes(bytes.get_ref()), ImageState::Failed));
        }

        // Case `decode_refuses_an_oversized_source_before_image_parsing`.
        {
            let path =
                std::env::temp_dir().join(format!("slides-oversize-{}.bin", uuid::Uuid::new_v4()));
            std::fs::write(&path, vec![0_u8; MAX_IMAGE_FILE_BYTES + 1])
                .expect("write oversize source");
            assert!(matches!(
                decode(&ContentStore::memory(), path.to_str().expect("utf-8 path")),
                ImageState::Failed
            ));
            let _ = std::fs::remove_file(path);
        }

        // Case `image_store_never_leaves_a_failed_worker_request_loading`.
        {
            let store = ImageStore {
                content: ContentStore::memory(),
                inner: RefCell::new(HashMap::new()),
                request_tx: None,
                rx: std::sync::mpsc::channel().1,
                ready_bytes: Cell::new(0),
                access: RefCell::new(HashMap::new()),
                access_clock: Cell::new(0),
                held: RefCell::default(),
                arriving: RefCell::default(),
            };
            assert!(matches!(
                store.get("/does/not/exist.png"),
                ImageState::Failed
            ));
            assert!(matches!(
                store.get("/does/not/exist.png"),
                ImageState::Failed
            ));
        }
    }
}
