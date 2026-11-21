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
    /// (`tilo-blob://<hash>`) or a picture carried in its own address (`data:image/png;base64,...`),
    /// and fetching nothing else.
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
            data_bytes(url)
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
        if let Some(image) = self.store.get(&picture.src) {
            return Some(image);
        }
        // A picture that carries its own bytes needs no store to load it, so a surface painting
        // outside the frame that answers its store (the shell's floating cards, drawn once when
        // they change) shows it on that frame.
        carried(&picture.src)
    }

    /// Whether the picture at `picture`'s address has failed to load: its bytes are not here, or do not
    /// decode.
    #[must_use]
    pub fn failed(&self, picture: &PictureSource) -> bool {
        !picture.is_empty() && self.store.failed(&picture.src)
    }
}

/// The picture a `data:` address carries, decoded here and kept for the next frame: `None` for any
/// other address, which only its store loads.
pub(crate) fn carried(url: &str) -> Option<PenikoImage> {
    thread_local! {
        static LAST: std::cell::RefCell<Option<(String, PenikoImage)>> =
            const { std::cell::RefCell::new(None) };
    }
    let kept = LAST.with(|last| {
        last.borrow()
            .as_ref()
            .filter(|(address, _)| address == url)
            .map(|(_, image)| image.clone())
    });
    if kept.is_some() {
        return kept;
    }
    let bytes = data_bytes(url)?;
    // Through the toolkit's decoders, which a program may fetch on first
    // use: until they are in, the picture keeps its place.
    let picture = (crate::image_decoders::decoders()?.page)(&bytes)?;
    LAST.with(|last| *last.borrow_mut() = Some((url.to_owned(), picture.clone())));
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

/// Keep the picture `bytes`, named `name`, in this process's content store and answer where it
/// is: what an editor does with a picture the person pasted or dropped into a document. Only a
/// picture is kept (PNG, JPEG, GIF, WebP or SVG, read from its own bytes), and only up to
/// [`MAX_PICTURE_BYTES`]; `None` otherwise, or when this process holds no content store (the web
/// answers its host for one) or the store refused it.
#[must_use]
pub fn keep_picture(name: &str, bytes: &[u8]) -> Option<KeptPicture> {
    let format = tilo_content::image_format(bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_PICTURE_BYTES {
        return None;
    }
    let store = device_store()?;
    let hash = store.put(bytes).ok()?;
    Some(KeptPicture {
        name: if name.trim().is_empty() {
            format!("Pasted image.{}", format.extension)
        } else {
            name.to_owned()
        },
        address: format!("{BLOB_SCHEME}{hash}"),
    })
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
/// dropped on a window. `None` when the file is no picture, holds more than [`MAX_PICTURE_BYTES`],
/// or cannot be read.
#[must_use]
pub fn dropped_picture(path: &std::path::Path) -> Option<KeptPicture> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)?;
    let (dir, file) = tilo_files::file_at(path).ok()?;
    let bytes = tilo_files::blocking::read_bounded(
        &dir,
        &file,
        MAX_PICTURE_BYTES.saturating_add(1),
    )
    .ok()??;
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

    /// A picture pasted or dropped into a document is kept only when it is one, and only up to
    /// the size a document's picture may be: what the editor refuses, it refuses before anything
    /// is written.
    #[test]
    fn only_a_picture_of_a_document_s_size_is_kept() {
        let _env = tilo_test_env::lock();
        let pixels = png();
        // This process holds no device store, so a picture that fits answers nothing here — but
        // a file that is no picture, or one past the size, is refused before the store is asked.
        assert_eq!(
            keep_picture("words.txt", b"not a picture"),
            None,
            "a text file is no picture"
        );
        assert_eq!(
            keep_picture("huge.png", &[png(), vec![0; MAX_PICTURE_BYTES]].concat()),
            None,
            "a picture past what a document keeps is refused"
        );
        assert_eq!(keep_picture("fox.png", &pixels), None, "no store, no address");

        let directory = tilo_test_env::runtime_tempdir().expect("a place for the drop");
        let path = directory.path().join("fox.png");
        std::fs::write(&path, &pixels).expect("the picture is written");
        assert_eq!(
            dropped_picture(&path),
            None,
            "a picture read from a drop is kept in the store this process holds, which is none"
        );
        let words = directory.path().join("words.txt");
        std::fs::write(&words, b"not a picture").expect("the file is written");
        assert_eq!(dropped_picture(&words), None, "a dropped text file is no picture");
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
}
