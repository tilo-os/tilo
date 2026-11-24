//! The pictures a window's components show: each [`Picture`] as the toolkit's decoded image. A picture
//! is an address ([`Picture::src`]): a content address (`tilo-blob://<hash>`) whose bytes are read from
//! the window's content store, or any other address an image store fetches. A picture whose bytes are
//! not here yet keeps its place: the component paints its placeholder until [`Pictures::get`] answers,
//! and an address that cannot be read fails visibly, as any other image does.

use vello::peniko::ImageData as PenikoImage;

use crate::ImageStore;
use crate::image_store::Fetcher;
use crate::widget::PictureSource;

/// How an app names a picture by its content address.
pub const BLOB_SCHEME: &str = "tilo-blob://";

/// What a component showing a picture reads: the store its bytes load through (which caches a decoded
/// picture by its address), and what a window's pictures are.
#[derive(Clone)]
pub struct Pictures {
    store: ImageStore,
}

impl std::fmt::Debug for Pictures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pictures").finish_non_exhaustive()
    }
}

impl Pictures {
    /// The pictures this window reads, loaded by `store`.
    #[must_use]
    pub fn new(store: ImageStore) -> Self {
        Self { store }
    }

    /// The window's pictures: loaded through the store the toolkit builds for the window (the one its
    /// host serves), with content addresses read from the process's content store.
    #[must_use]
    pub fn window() -> Self {
        thread_local! {
            static WINDOW: std::cell::OnceCell<Pictures> = const { std::cell::OnceCell::new() };
        }
        WINDOW.with(|window| window.get_or_init(Self::own).clone())
    }

    /// The pictures this window reads, loading their bytes from the content store the process holds
    /// (`tilo-blob://<hash>`), a picture carried in its own address
    /// (`data:image/png;base64,...`), or a remote one a host serves ([`set_remote_fetcher`]).
    fn own() -> Self {
        let fetch: Fetcher = std::sync::Arc::new(|url: &str| {
            if let Some(hash) = url.strip_prefix(BLOB_SCHEME) {
                let bytes = blob_bytes(hash);
                if bytes.is_none() {
                    // A content address this window cannot read is a card that keeps its
                    // placeholder: say which address failed.
                    tracing::debug!(%hash, "a window read a content address it does not hold");
                }
                return bytes;
            }
            data_bytes(url).or_else(|| remote_bytes(url))
        });
        Self::new(ImageStore::with_max_dimension(
            &fetch,
            PICTURE_MAX_DIMENSION,
        ))
    }

    /// The store it loads through, for a component that shows more than one picture.
    #[must_use]
    pub fn store(&self) -> &ImageStore {
        &self.store
    }

    /// The decoded picture at `picture`'s address, once its bytes are here: `None` while it loads (or
    /// when it names nothing, or failed to load).
    #[must_use]
    pub fn get(&self, picture: &PictureSource) -> Option<PenikoImage> {
        if picture.is_empty() {
            return None;
        }
        // A stream's newest frame is held by the device's engine, not read from a store, and is
        // asked for again on every draw: a window that draws late shows the newest frame. It is
        // answered before the store, which would fetch an address it can never read.
        if let Some(slot) = crate::live_pictures::slot_of(&picture.src) {
            return crate::live_pictures::get(slot);
        }
        // A picture that carries its own bytes needs no store to load it: it is decoded here,
        // before the store is asked, so it shows on the frame it is set and no background load
        // asks for bytes the address already holds.
        if let Some(image) = carried(&picture.src) {
            return Some(image);
        }
        self.store.get(&picture.src)
    }

    /// The decoded picture at `picture`'s address when it has already loaded, asking for nothing:
    /// what a map draws of a coarser tile under one still on its way.
    #[must_use]
    pub fn loaded(&self, picture: &PictureSource) -> Option<PenikoImage> {
        self.store
            .loaded(&picture.src)
            .or_else(|| carried(&picture.src))
    }

    /// Whether the picture at `picture`'s address has failed to load: its bytes are not here, or do not
    /// decode.
    #[must_use]
    pub fn failed(&self, picture: &PictureSource) -> bool {
        if crate::live_pictures::is_live(&picture.src) {
            // A stream that has not sent its first frame yet is loading, never failed.
            return false;
        }
        !picture.is_empty() && self.store.failed(&picture.src)
    }
}

/// The picture a `data:` address carries, decoded here and kept for the next frame: `None` for any
/// other address, which only its store loads.
pub(crate) fn carried(url: &str) -> Option<PenikoImage> {
    /// How many carried pictures are kept: a view that shows several in one window (a page of
    /// photographs carried in their addresses) decodes each once, not once a frame.
    const KEPT: usize = 16;
    thread_local! {
        /// The carried pictures decoded here, most recently shown first.
        static CARRIED: std::cell::RefCell<Vec<(String, PenikoImage)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let kept = CARRIED.with_borrow_mut(|carried| {
        let at = carried.iter().position(|(address, _)| address == url)?;
        let entry = carried.remove(at);
        let image = entry.1.clone();
        carried.insert(0, entry);
        Some(image)
    });
    if kept.is_some() {
        return kept;
    }
    let bytes = data_bytes(url)?;
    let picture = crate::image_decoders::page(&bytes)?;
    CARRIED.with_borrow_mut(|carried| {
        carried.insert(0, (url.to_owned(), picture.clone()));
        carried.truncate(KEPT);
    });
    Some(picture)
}

/// The bytes of the content at `hash`, from the content store this process holds (the launcher's device
/// store): `None` where the build has none (the web) or it is not here.
#[cfg(feature = "native")]
fn blob_bytes(hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = crate::content::device().and_then(|store| store.read(hash)) {
        return Some(bytes);
    }
    lent_content()?.read(hash)
}

/// The store the windows this process draws read a content address from when the launcher handed the
/// process no store of its own: the session's document media, which the session lends while it draws
/// ([`lend_content`]).
#[cfg(feature = "native")]
static LENT_CONTENT: std::sync::RwLock<Option<tilo_content::ContentStore>> =
    std::sync::RwLock::new(None);

#[cfg(feature = "native")]
fn lent_content() -> Option<tilo_content::ContentStore> {
    LENT_CONTENT.read().ok().and_then(|lent| lent.clone())
}

/// Let the windows this process draws read their content addresses from `store`: what a session lends
/// the windows it draws in its own process, once it knows the store the workspaces' content is read
/// through. The pictures keep the store they were built with, so what this changes is where their bytes
/// come from, read afresh each time a picture is asked for.
#[cfg(feature = "native")]
pub fn lend_content(store: Option<tilo_content::ContentStore>) {
    let Ok(mut lent) = LENT_CONTENT.write() else {
        return;
    };
    let fresh = lent.is_none() && store.is_some();
    *lent = store;
    drop(lent);
    if fresh {
        // A picture asked for before one answered failed, and a failure is kept so it is not
        // asked for every frame: what failed against no store is asked for again now.
        Pictures::window().store().forget_failed();
    }
}

/// Where a picture whose address is remote comes from: the process's own network, as the host
/// serving the windows reads it. A host installs it once (`set_remote_fetcher`), and the windows
/// this process draws then show pictures by their URL: a map's tiles, a site's pictures. It runs
/// on the store's worker, never the drawing thread, and the host's own sandbox decides what a
/// request reaches.
static REMOTE: std::sync::RwLock<Option<Fetcher>> = std::sync::RwLock::new(None);

/// Serve the windows this process draws their remote pictures with `fetch`: it answers a URL's
/// bytes, or `None` where it cannot reach or read them. `None` takes it away, and a picture whose
/// remote address failed is asked for again once one is installed.
pub fn set_remote_fetcher(fetch: Option<Fetcher>) {
    let Ok(mut remote) = REMOTE.write() else {
        return;
    };
    let fresh = remote.is_none() && fetch.is_some();
    *remote = fetch;
    drop(remote);
    if fresh {
        Pictures::window().store().forget_failed();
    }
}

/// What the host serving this process's windows answers for `url`, or `None` where it serves no
/// remote picture or the address is not one.
fn remote_bytes(url: &str) -> Option<Vec<u8>> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let fetch = REMOTE.read().ok().and_then(|remote| remote.clone())?;
    fetch(url)
}

/// The bytes of the content at `hash`: a web page reads no content store, so a content address there
/// resolves to nothing until its host serves it.
#[cfg(not(feature = "native"))]
fn blob_bytes(hash: &str) -> Option<Vec<u8>> {
    let _ = hash;
    None
}

/// The most a picture pasted or dropped into a document may hold: the size a document keeps a
/// picture at, which a `File` of it may answer.
pub const MAX_PICTURE_BYTES: usize = 1024 * 1024;

/// A picture taken in from a paste or a drop, as a document keeps it: the name it came in as, and
/// the address of its bytes (`tilo-blob://<hash>`), which every device of the workspace reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptPicture {
    /// What the picture is called, extension included (`Pasted image.png`).
    pub name: String,
    /// Where its bytes are: a content address.
    pub address: String,
}

/// Why a picture pasted or dropped into a document was not kept: what a window says where the
/// person did it, so a paste or a drop is never lost silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PictureRefusal {
    /// The bytes are no picture this program reads.
    NotAPicture,
    /// The picture kept more than [`MAX_PICTURE_BYTES`] even after being made smaller.
    TooLarge,
    /// This process holds no content store (the web answers its host for one).
    NoStore,
    /// The file could not be read from where it was dropped.
    Unreadable(String),
    /// The content store refused the bytes.
    Store(String),
}

impl PictureRefusal {
    /// Why the picture was not kept, in the person's words.
    #[must_use]
    pub fn words(&self) -> String {
        use crate::i18n::{widget_text, widget_text_with};
        match self {
            Self::NotAPicture => widget_text("picture-not-a-picture"),
            Self::TooLarge => widget_text("picture-too-large"),
            Self::NoStore => widget_text("picture-no-store"),
            Self::Unreadable(reason) | Self::Store(reason) => widget_text_with(
                "picture-not-kept",
                Some(&fluent::FluentArgs::from_iter([(
                    "reason",
                    reason.as_str(),
                )])),
            ),
        }
    }
}

/// Keep the picture `bytes`, named `name`, in this process's content store and answer where it
/// is: what an editor does with a picture the person pasted or dropped into a document. Only a
/// picture is kept (PNG, JPEG, GIF, WebP or SVG, read from its own bytes); one larger than
/// [`MAX_PICTURE_BYTES`] is made smaller first ([`shrink_picture`]), so the person never loses a
/// paste to its size.
///
/// # Errors
///
/// [`PictureRefusal`]: the bytes are no picture, the picture cannot be made to fit, this process
/// holds no content store, or the store refused it.
pub fn keep_picture(name: &str, bytes: &[u8]) -> Result<KeptPicture, PictureRefusal> {
    let format = if bytes.is_empty() {
        None
    } else {
        tilo_content::image_format(bytes)
    };
    let Some(format) = format else {
        return Err(PictureRefusal::NotAPicture);
    };
    let named = if name.trim().is_empty() {
        format!("Pasted image.{}", format.extension)
    } else {
        name.to_owned()
    };
    // A picture past what a document keeps is re-encoded and scaled down until it fits: what a
    // screenshot or a camera's photograph is, and still what the person meant to put in.
    let (name, bytes) = if bytes.len() <= MAX_PICTURE_BYTES {
        (named, bytes.to_vec())
    } else {
        let smaller = shrink_picture(bytes).ok_or(PictureRefusal::TooLarge)?;
        let format = tilo_content::image_format(&smaller).ok_or(PictureRefusal::TooLarge)?;
        (with_extension(&named, format.extension), smaller)
    };
    let store = device_store().ok_or(PictureRefusal::NoStore)?;
    let hash = store
        .put(&bytes)
        .map_err(|error| PictureRefusal::Store(error.to_string()))?;
    Ok(KeptPicture {
        name,
        address: format!("{BLOB_SCHEME}{hash}"),
    })
}

/// `name` with its extension replaced by `extension`: a picture made smaller is kept as the
/// format it was written as now, and its name says so (`photo.png` a JPEG becomes `photo.jpg`).
fn with_extension(name: &str, extension: &str) -> String {
    let stem = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or(name, |(stem, _)| stem);
    format!("{stem}.{extension}")
}

/// `bytes` re-encoded and scaled down, keeping its proportions, until it is no more than
/// [`MAX_PICTURE_BYTES`]: a picture with alpha stays a PNG (a lossless copy that keeps it), and
/// one without becomes a JPEG. `None` when it cannot be made to fit, or is no picture this
/// program decodes (an SVG).
fn shrink_picture(bytes: &[u8]) -> Option<Vec<u8>> {
    use crate::image_codec::{Encoding, decode_now, encode_now};
    let mut image = decode_now(bytes, Some(PICTURE_MAX_DIMENSION)).ok()?;
    let encoding = if image.has_alpha() {
        Encoding::Png
    } else {
        Encoding::Jpeg { quality: 85 }
    };
    for _ in 0..8 {
        let encoded = encode_now(&image, encoding).ok()?;
        if encoded.len() <= MAX_PICTURE_BYTES {
            return Some(encoded);
        }
        if image.width < 64 || image.height < 64 {
            break;
        }
        image = image.resized(
            image.width.saturating_mul(3) / 4,
            image.height.saturating_mul(3) / 4,
        );
    }
    None
}

/// The store a picture is kept in: the device content store the launcher handed this process, and
/// none in a build that keeps no such store (the web), where a host keeps the picture instead.
#[cfg(feature = "native")]
fn device_store() -> Option<crate::content::ContentStore> {
    crate::content::device()
}

#[cfg(not(feature = "native"))]
fn device_store() -> Option<crate::content::ContentStore> {
    None
}

/// The picture the file at `path` holds, kept and answered as [`keep_picture`] does: a picture file
/// dropped on a window. A picture larger than a document keeps is made smaller first, as a paste
/// is; one past [`crate::clipboard::MAX_PASTED_IMAGE_BYTES`] is not read at all.
///
/// # Errors
///
/// [`PictureRefusal`]: the file cannot be read, or [`keep_picture`]'s reason.
pub fn dropped_picture(path: &std::path::Path) -> Result<KeptPicture, PictureRefusal> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PictureRefusal::NotAPicture)?
        .to_owned();
    let (dir, file) =
        tilo_files::file_at(path).map_err(|error| PictureRefusal::Unreadable(error.to_string()))?;
    let bytes = tilo_files::blocking::read_bounded(
        &dir,
        &file,
        crate::clipboard::MAX_PASTED_IMAGE_BYTES.saturating_add(1),
    )
    .map_err(|error| match error {
        tilo_files::FsError::FileTooLarge => PictureRefusal::TooLarge,
        other => PictureRefusal::Unreadable(other.to_string()),
    })?;
    keep_picture(&name, &bytes)
}

/// The bytes a `data:` address carries (`data:image/png;base64,<data>`), when it is one: a picture that
/// travels in its own address, which a document or a demo can hold.
#[must_use]
pub fn data_bytes(url: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(data.trim().as_bytes())
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

/// The largest a picture is decoded to, in each dimension: a picture shown in a window or a card is
/// never larger, and a screen-sized cap keeps a huge source from filling the cache.
const PICTURE_MAX_DIMENSION: u32 = 4096;

/// One picture a widget shows, by its address: loaded through the window's pictures the first frame it
/// is here, and kept while the address stays.
#[derive(Clone, Default)]
pub struct Shown {
    pictures: Option<Pictures>,
    picture: Option<PictureSource>,
    image: Option<PenikoImage>,
}

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown")
            .field("picture", &self.picture)
            .field("loaded", &self.image.is_some())
            .finish_non_exhaustive()
    }
}

impl Shown {
    /// Show `picture` instead, through `pictures`; `true` when that is a picture other than the one
    /// shown (its bytes load on the next read).
    pub fn set(&mut self, pictures: Pictures, picture: PictureSource) -> bool {
        let same = self
            .picture
            .as_ref()
            .is_some_and(|shown| shown.src == picture.src)
            && self.pictures.is_some();
        if same {
            self.picture = Some(picture);
            return false;
        }
        self.pictures = Some(pictures);
        self.picture = Some(picture);
        self.image = None;
        true
    }

    /// Take the picture's bytes, once they are here; `true` when that changed what it shows.
    pub fn resolve(&mut self) -> bool {
        if self.image.is_some() {
            return false;
        }
        let (Some(pictures), Some(picture)) = (&self.pictures, &self.picture) else {
            return false;
        };
        let Some(image) = pictures.get(picture) else {
            return false;
        };
        self.image = Some(image);
        true
    }

    /// The decoded picture, once its bytes are here.
    #[must_use]
    pub fn image(&self) -> Option<&PenikoImage> {
        self.image.as_ref()
    }

    /// Whether it is a picture whose bytes are not here and cannot be: it shows the error mark.
    #[must_use]
    pub fn failed(&self) -> bool {
        match (&self.pictures, &self.picture) {
            (Some(pictures), Some(picture)) if self.image.is_none() => pictures.failed(picture),
            _ => false,
        }
    }

    /// Whether it waits for its picture: it asks for a frame until the bytes are here or fail.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.picture.is_some() && self.image.is_none() && !self.failed()
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    #![allow(clippy::expect_used, reason = "in a test this is the assertion")]

    use super::*;

    /// A session's own process — the shell drawing the windows it runs itself — holds no device store
    /// of its own: the launcher hands one only to the apps it starts.
    #[test]
    fn a_content_address_reads_from_the_store_the_session_lent() {
        let _env = tilo_test_env::lock();
        let still = b"a still, as the connector kept it";
        let store = tilo_content::ContentStore::memory();
        let hash = store.put(still).expect("the still is kept");

        lend_content(None);
        assert_eq!(
            blob_bytes(&hash),
            None,
            "a process handed no store reads no content address"
        );

        lend_content(Some(store));
        assert_eq!(
            blob_bytes(&hash),
            Some(still.to_vec()),
            "a place draws what the session's media served"
        );
        lend_content(None);
    }

    /// A picture pasted or dropped into a document is kept only when it is one; one larger than
    /// a document keeps is made smaller, never dropped for its size.
    #[test]
    fn only_a_picture_is_kept_and_a_large_one_is_made_smaller_first() {
        let _env = tilo_test_env::lock();
        let pixels = png();
        // This process holds no device store, so a picture that fits answers nothing here — but
        // a file that is no picture is refused before the store is asked.
        assert_eq!(
            keep_picture("words.txt", b"not a picture"),
            Err(PictureRefusal::NotAPicture),
            "a text file is no picture"
        );
        assert_eq!(
            keep_picture(
                "huge.jpg",
                &[&[0xff, 0xd8, 0xff][..], &vec![0; MAX_PICTURE_BYTES]].concat()
            ),
            Err(PictureRefusal::TooLarge),
            "a picture past what a document keeps, which cannot be decoded to be made smaller"
        );
        assert_eq!(
            keep_picture("fox.png", &pixels),
            Err(PictureRefusal::NoStore),
            "no store, no address"
        );

        let directory = tilo_test_env::runtime_tempdir().expect("a place for the drop");
        let path = directory.path().join("fox.png");
        std::fs::write(&path, &pixels).expect("the picture is written");
        assert_eq!(
            dropped_picture(&path),
            Err(PictureRefusal::NoStore),
            "a picture read from a drop is kept in the store this process holds, which is none"
        );
        let words = directory.path().join("words.txt");
        std::fs::write(&words, b"not a picture").expect("the file is written");
        assert_eq!(
            dropped_picture(&words),
            Err(PictureRefusal::NotAPicture),
            "a dropped text file is no picture"
        );
    }

    /// A picture past the size a document keeps is re-encoded and scaled down, keeping its
    /// proportions, so a screenshot or a photograph still goes in.
    #[test]
    fn a_picture_past_the_size_is_made_smaller_keeping_its_proportions() {
        let big = big_png();
        assert!(
            big.len() > MAX_PICTURE_BYTES,
            "the picture starts larger than a document keeps"
        );
        let (_, wide, tall) = tilo_content::image_size(&big).expect("the picture says its size");
        let smaller = shrink_picture(&big).expect("a picture can be made to fit");
        assert!(
            smaller.len() <= MAX_PICTURE_BYTES,
            "what is kept is a document's size"
        );
        let (format, width, height) =
            tilo_content::image_size(&smaller).expect("what is kept is still a picture");
        assert_eq!(format.mime, "image/jpeg", "no alpha, so a JPEG");
        let before = f64::from(wide) / f64::from(tall);
        let after = f64::from(width) / f64::from(height);
        assert!(
            (before - after).abs() < 0.02,
            "the proportions stay: {before} became {after}"
        );
    }

    /// The name says what the bytes are now: a picture re-encoded as a JPEG is a `.jpg`.
    #[test]
    fn a_smaller_picture_is_named_for_its_new_format() {
        assert_eq!(with_extension("photo.png", "jpg"), "photo.jpg");
        assert_eq!(
            with_extension("Pasted image.png", "webp"),
            "Pasted image.webp"
        );
        assert_eq!(with_extension("photo", "png"), "photo.png");
    }

    /// A 2x2 PNG, written out rather than drawn: the test needs no image library to say what a
    /// picture is.
    fn png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x16, b'I', b'D', b'A', b'T', 0x78,
            0x9c, 0x63, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x08, 0x00,
            0x01, 0x2c, 0x0a, 0x0b, 0x4e, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
            0x42, 0x60, 0x82,
        ]
    }

    /// A PNG larger than a document keeps, of random-looking pixels that do not compress away.
    fn big_png() -> Vec<u8> {
        use image::ImageEncoder as _;
        let (width, height) = (1200_u32, 900_u32);
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            rgba.extend_from_slice(&seed.to_le_bytes()[..3]);
            rgba.push(255);
        }
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .expect("the picture encodes");
        bytes
    }
}
