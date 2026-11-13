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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

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
const MAX_READY_IMAGE_BYTES: usize = 96 * 1024 * 1024;
/// How long a picture whose bytes are still arriving from another device
/// waits before it is read again.
const ARRIVING_RETRY: std::time::Duration = std::time::Duration::from_millis(500);
pub const ASSET_PREFIX: &str = "asset:blake3:";

pub fn app_content() -> ContentStore {
    ContentStore::memory()
}

/// A deck carries this content identity, never a machine-local file path:
/// the reference the storing side got, which carries the key of content the
/// workspace seals, so a later peer opens the bytes by it.
pub fn is_asset_id(id: &str) -> bool {
    id.strip_prefix(ASSET_PREFIX)
        .is_some_and(tilo_content::valid_reference)
}

/// Store supplied bytes in `content` and return the portable identifier that
/// is safe to place in the collaborative deck: the reference the store
/// answered, which a later peer requests the bytes by.
pub fn store_asset(content: &ContentStore, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_FILE_BYTES {
        return None;
    }
    // Do not create a CRDT reference unless the store accepted the bytes.
    match content.put(bytes) {
        Ok(reference) => Some(format!("{ASSET_PREFIX}{reference}")),
        Err(error) => {
            eprintln!("slides: content store refused an image: {error}");
            None
        }
    }
}

/// Resolve a deck asset only after checking both its bounded bytes and its
/// declared reference (the store verifies every read).  A missing or corrupt
/// blob is an ordinary unavailable asset, never an instruction to read an
/// arbitrary peer-provided filesystem path.
pub fn read_asset(content: &ContentStore, id: &str) -> Option<Vec<u8>> {
    if !is_asset_id(id) {
        return None;
    }
    let reference = id.strip_prefix(ASSET_PREFIX)?;
    let bytes = content.read(reference)?;
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

#[derive(Clone)]
pub enum ImageState {
    /// The file is being decoded off the frame, or its bytes are on their
    /// way from another device.
    Loading,
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

pub struct ImageStore {
    content: ContentStore,
    inner: RefCell<HashMap<String, ImageState>>,
    tx: Sender<(String, ImageState)>,
    decoding: Arc<AtomicUsize>,
    rx: Receiver<(String, ImageState)>,
    ready_bytes: Cell<usize>,
    access: RefCell<HashMap<String, u64>>,
    access_clock: Cell<u64>,
    held: RefCell<std::collections::HashSet<String>>,
    /// Pictures whose bytes another device is still sending this one, and
    /// when they were last asked for.
    arriving: RefCell<HashMap<String, ui::time::Instant>>,
}

impl ImageStore {
    pub fn new(content: ContentStore) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(String, ImageState)>();
        ImageStore {
            content,
            inner: RefCell::new(HashMap::new()),
            tx,
            decoding: Arc::default(),
            rx,
            ready_bytes: Cell::new(0),
            access: RefCell::new(HashMap::new()),
            access_clock: Cell::new(0),
            held: RefCell::default(),
            arriving: RefCell::default(),
        }
    }

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
        if self.decoding.fetch_add(1, Ordering::AcqRel) >= DECODE_QUEUE_CAPACITY {
            self.decoding.fetch_sub(1, Ordering::AcqRel);
            map.insert(path.to_string(), ImageState::Failed);
            return ImageState::Failed;
        }
        map.insert(path.to_string(), ImageState::Loading);
        let (tx, decoding, path) = (self.tx.clone(), Arc::clone(&self.decoding), path.to_owned());
        let decoded = decode(self.content.clone(), path.clone());
        ui::rt::app().spawn_then(decoded, move |state| {
            decoding.fetch_sub(1, Ordering::AcqRel);
            let _ = tx.send((path, state));
        });
        ImageState::Loading
    }

    /// Decode `path` synchronously and cache the result: a screenshot, whose
    /// frame must have the pixels before it renders. Never called on the
    /// live UI thread.
    pub fn load_blocking(&self, path: &str) {
        let state = ui::rt::block_on(decode(self.content.clone(), path.to_owned()))
            .unwrap_or(ImageState::Failed);
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

async fn decode(content: ContentStore, path: String) -> ImageState {
    let path = path.as_str();
    // This is deliberately categorical. The string arrived through a shared
    // CRDT and must never become a peer-controlled local-path probe. Screenshot
    // fixtures use `store_asset` first, exactly as the real portal does.
    if !is_asset_id(path) {
        return ImageState::Failed;
    }
    let Some(bytes) = read_asset(&content, path) else {
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
    decode_bytes(&bytes).await
}

/// Pure bounded decode of already admitted bytes; no files, workers or stores.
pub(crate) async fn decode_bytes(bytes: &[u8]) -> ImageState {
    decode_bytes_with_budget(bytes, MAX_READY_IMAGE_BYTES).await
}

pub(crate) async fn decode_bytes_with_budget(bytes: &[u8], budget: usize) -> ImageState {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_FILE_BYTES {
        return ImageState::Failed;
    }
    let max_decode_bytes = MAX_IMAGE_DECODE_BYTES.min(u64::try_from(budget).unwrap_or(u64::MAX));
    // The header's size bounds the decode before any pixel is allocated.
    let Some((w, h)) = ui::image_codec::dimensions(bytes) else {
        return ImageState::Failed;
    };
    let pixels = u64::from(w).saturating_mul(u64::from(h));
    if w == 0
        || h == 0
        || w > MAX_IMAGE_DIMENSION
        || h > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
        || pixels.saturating_mul(4) > max_decode_bytes
    {
        return ImageState::Failed;
    }
    let Ok(decoded) = ui::image_codec::decode(bytes, None).await else {
        return ImageState::Failed;
    };
    if decoded.width != w
        || decoded.height != h
        || decoded.rgba.len() > usize::try_from(MAX_IMAGE_DECODE_BYTES).unwrap_or(usize::MAX)
    {
        return ImageState::Failed;
    }
    let blob = Blob::new(std::sync::Arc::new(decoded.rgba));
    ImageState::Ready(ui::raster::rgba8(blob, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_content::{
        ContentBackend, ContentError, ContentReader, ContentState, MemoryBackend, Stored,
    };

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
            tilo_test_env::wait_for("the decode to settle", || {
                images.pump();
                let state = images.get(&src);
                (!images.arriving.borrow().is_empty() || !matches!(state, ImageState::Loading))
                    .then_some(state)
            })
        };
        assert!(
            matches!(settle(&mut images), ImageState::Loading),
            "bytes on their way are loading, not a broken picture"
        );
        arrived.store(true, Ordering::SeqCst);
        let ready = tilo_test_env::wait_for("the picture to appear", || {
            images.pump();
            match images.get(&src) {
                ImageState::Ready(ready) => Some(ready),
                _ => None,
            }
        });
        assert_eq!((ready.width, ready.height), (3, 2));
    }

    /// Frozen/live decode budgets and limits, and failed worker requests never stay loading.
    #[test]
    fn image_decoding_is_bounded_and_never_left_loading() {
        {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            assert!(matches!(
                ui::rt::block_on(decode_bytes_with_budget(bytes.get_ref(), 15)).unwrap(),
                ImageState::Failed
            ));
            assert!(matches!(
                ui::rt::block_on(decode_bytes_with_budget(bytes.get_ref(), 16)).unwrap(),
                ImageState::Ready(_)
            ));
        }

        {
            assert!(matches!(
                ui::rt::block_on(decode_bytes(&vec![0; MAX_IMAGE_FILE_BYTES + 1])).unwrap(),
                ImageState::Failed
            ));
            assert!(matches!(
                ui::rt::block_on(decode_bytes(b"corrupt")).unwrap(),
                ImageState::Failed
            ));
            let image =
                image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, MAX_IMAGE_DIMENSION + 1));
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            assert!(matches!(
                ui::rt::block_on(decode_bytes(bytes.get_ref())).unwrap(),
                ImageState::Failed
            ));
        }

        {
            let path =
                std::env::temp_dir().join(format!("slides-oversize-{}.bin", uuid::Uuid::new_v4()));
            std::fs::write(&path, vec![0_u8; MAX_IMAGE_FILE_BYTES + 1])
                .expect("write oversize source");
            assert!(matches!(
                ui::rt::block_on(decode(
                    ContentStore::memory(),
                    path.to_str().expect("utf-8 path").to_owned()
                ))
                .unwrap(),
                ImageState::Failed
            ));
            let _ = std::fs::remove_file(path);
        }

        // Case `image_store_never_leaves_a_refused_request_loading`: with as
        // many decodes under way as it runs, a new picture is a settled
        // failure, not a spinner nothing will end.
        {
            let store = ImageStore::new(ContentStore::memory());
            store
                .decoding
                .store(DECODE_QUEUE_CAPACITY, std::sync::atomic::Ordering::SeqCst);
            let src = format!("{ASSET_PREFIX}{}", "0".repeat(64));
            assert!(matches!(store.get(&src), ImageState::Failed));
            assert!(matches!(store.get(&src), ImageState::Failed));
        }
    }

    /// A picture kept in a workspace that seals is ciphertext in the store,
    /// and the deck's asset id is the reference that carries its key: a
    /// member whose device holds only the ciphertext opens it by the id, and
    /// a picture named by a plain hash, as decks wrote before content was
    /// sealed, still reads.
    #[test]
    fn a_picture_is_kept_sealed_and_its_asset_id_opens_it_for_a_member() {
        let png = ui::png::encode_rgba8(8, 6, &[230, 30, 20, 255].repeat(48)).expect("encodes");
        let writer = ContentStore::memory_in_workspace().sealing();
        let id = store_asset(&writer, &png).expect("the picture is kept");
        let reference = id.strip_prefix(ASSET_PREFIX).expect("an asset id");
        assert!(tilo_content::is_sealed(reference), "{id}");
        assert!(
            !writer.has(&tilo_content::hash_bytes(&png)),
            "the store holds no plaintext"
        );

        let address = tilo_content::content_address(reference).expect("a reference");
        let carried = writer.read(&address).expect("the ciphertext");
        assert_ne!(carried, png, "the ciphertext is not the picture");
        let member = ContentStore::memory_in_workspace();
        member.put_public(&carried).expect("kept as carried");
        assert_eq!(read_asset(&member, &id), Some(png.clone()));

        // A deck that names a plain hash, as one written before content was
        // sealed does, reads the bytes the same device kept under it.
        let plain = member.put(&png).expect("kept in the clear");
        assert_eq!(
            read_asset(&member, &format!("{ASSET_PREFIX}{plain}")),
            Some(png)
        );
    }
}
