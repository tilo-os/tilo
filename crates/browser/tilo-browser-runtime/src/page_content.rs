//! The content the page's windows hold: the pictures and attachments their
//! records reference, by hash, in the page's memory. Every window of the
//! page shares it, so content one fetched is there for the others; the
//! framework joins it to each window's record route, so content a record
//! references and the page lacks is asked of the room, and its pieces are
//! verified before they are kept here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_content::{ContentLimits, ContentStore, MemoryBackend};

/// How an app names an image by its content address.
const BLOB_SCHEME: &str = "tilo-blob://";

/// Most the page keeps: a tab is not a disk.
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    static PAGE_CONTENT: ContentStore = ContentStore::new(MemoryBackend::new(ContentLimits {
        max_total_bytes: MAX_TOTAL_BYTES,
        ..ContentLimits::default()
    }));
}

/// The page's content store, lent to each window it opens.
pub(crate) fn page_content() -> ContentStore {
    PAGE_CONTENT.with(Clone::clone)
}

/// The window each image loader of a mount draws for, by the loader's place
/// in the mount's list: its content store is the one the window reads its
/// record's content from, once the window is built.
pub(crate) type ImageOwners = Rc<RefCell<HashMap<usize, ui::image_store::ServedWindow>>>;

/// Serve the image stores of the window whose wake scope `window` is from
/// the page: each store the window builds, as it is constructed or later,
/// and each store of a window opened in its place, gets a loader the page
/// drives, in `loaders`, drawing for the window that built it.
pub(crate) fn serve_window(
    window: &ui::wake::WakeHandle,
    loaders: &Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    owners: &ImageOwners,
) {
    let (loaders, owners) = (Rc::clone(loaders), Rc::clone(owners));
    ui::ImageStore::serve_window(
        window,
        Rc::new(move |max_dimension, drawn_for| {
            let (image, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
            let mut loaders = loaders.borrow_mut();
            owners.borrow_mut().insert(loaders.len(), drawn_for.clone());
            loaders.push(loader);
            image
        }),
    );
}

/// The content store of the window `loader` draws for, and the content
/// address `url` names; `None` for any other image.
fn blob(owners: &ImageOwners, loader: usize, url: &str) -> Option<(ContentStore, String)> {
    let hash = url.strip_prefix(BLOB_SCHEME)?;
    let store = owners.borrow().get(&loader)?.content()?;
    Some((store, hash.to_owned()))
}

/// Whether the page can hand image `url` to the page's decoder now: any
/// image but a content address is; one is once its bytes are here, and
/// asking for it otherwise asks the window's room.
pub(crate) fn image_ready(owners: &ImageOwners, loader: usize, url: &str) -> bool {
    if !url.starts_with(BLOB_SCHEME) {
        return true;
    }
    let Some((store, hash)) = blob(owners, loader, url) else {
        web_sys::console::warn_1(
            &format!("content image {url}: loader {loader} draws for no window").into(),
        );
        return false;
    };
    if store.has(&hash) {
        return true;
    }
    // Opening content the store lacks asks the record's route for it.
    let _ = store.read(&hash);
    false
}

/// The bytes of content-addressed image `url`, when they are here.
pub(crate) fn image_bytes(owners: &ImageOwners, loader: usize, url: &str) -> Option<Vec<u8>> {
    let (store, hash) = blob(owners, loader, url)?;
    store.read(&hash)
}
