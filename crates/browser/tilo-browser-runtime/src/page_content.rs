//! The content the page's windows hold: the pictures and attachments their
//! records reference, by hash, in the page's memory. Every window of the
//! page shares it, so content one fetched is there for the others; the
//! framework joins it to each window's record route, so content a record
//! references and the page lacks is asked of the room, and its pieces are
//! verified before they are kept here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use tilo_content::{ContentLimits, ContentStore, MemoryBackend};
use ui::collaboration::record_content::{ContentLane, RecordContent};

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

/// The window (app and record) each image loader of a mount draws for, by
/// the loader's place in the mount's list.
pub(crate) type ImageOwners = Rc<RefCell<HashMap<usize, (String, String)>>>;

/// The loaders at `loaders` draw for record `record` of app `app`.
pub(crate) fn own_loaders(owners: &ImageOwners, loaders: Range<usize>, app: &str, record: &str) {
    let mut owners = owners.borrow_mut();
    for loader in loaders {
        owners.insert(loader, (app.to_owned(), record.to_owned()));
    }
}

/// The content store of the window `loader` draws for, on its record's
/// route, and the content address `url` names; `None` for any other image.
fn blob(owners: &ImageOwners, loader: usize, url: &str) -> Option<(ContentStore, String)> {
    let hash = url.strip_prefix(BLOB_SCHEME)?;
    let owners = owners.borrow();
    let (app, record) = owners.get(&loader)?;
    let store = RecordContent::store(page_content(), ContentLane::of(app, record));
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
    if let Some((app, record)) = owners.borrow().get(&loader) {
        web_sys::console::debug_1(
            &format!("content image {hash}: asked on {app} record {record}").into(),
        );
    }
    false
}

/// The bytes of content-addressed image `url`, when they are here.
pub(crate) fn image_bytes(owners: &ImageOwners, loader: usize, url: &str) -> Option<Vec<u8>> {
    let (store, hash) = blob(owners, loader, url)?;
    store.read(&hash)
}
