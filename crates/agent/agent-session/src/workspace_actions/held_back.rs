//! A document a test keeps away from the workspace's service: not held, as far as the service
//! knows, and asked for without being sent until the test lets it go, when it is there as one
//! that just arrived. A part on its way is what a restarted server, a slow network or a
//! large chain gives a request to wait for; a test holds one back to see what the service does
//! meanwhile.

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

#[derive(Default)]
struct Held {
    /// The documents held back, by id, and whether the service looked for each since.
    documents: HashMap<String, bool>,
    /// How many times a document was held back or let go: what the service listed of its
    /// workspace's documents is listed again once this moved.
    moved: u64,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

fn with<T>(change: impl FnOnce(&mut Held) -> T) -> T {
    change(HELD.lock_recover().get_or_insert_with(Held::default))
}

/// The document `document` held back from every workspace's service of this process until
/// this is dropped.
#[must_use = "the document is held back only while this is kept"]
pub struct HeldBack {
    document: String,
}

impl HeldBack {
    /// Hold `document` (a chain part's id, as its app names it) back.
    pub fn new(document: &str) -> Self {
        with(|held| {
            held.documents.insert(document.to_owned(), false);
            held.moved = held.moved.wrapping_add(1);
        });
        Self {
            document: document.to_owned(),
        }
    }

    /// Whether the service looked for it since it was held back.
    #[must_use]
    pub fn looked_for(&self) -> bool {
        with(|held| held.documents.get(&self.document).copied().unwrap_or(false))
    }
}

impl Drop for HeldBack {
    fn drop(&mut self) {
        with(|held| {
            held.documents.remove(&self.document);
            held.moved = held.moved.wrapping_add(1);
        });
    }
}

/// Whether `document` is held back: the service holds none of it, and a request that looks
/// for it is told so, and counted as having looked.
pub(crate) fn hidden(document: &str) -> bool {
    with(|held| {
        held.documents
            .get_mut(document)
            .map(|looked| *looked = true)
            .is_some()
    })
}

/// How many times a document was held back or let go.
pub(crate) fn moved() -> u64 {
    with(|held| held.moved)
}
