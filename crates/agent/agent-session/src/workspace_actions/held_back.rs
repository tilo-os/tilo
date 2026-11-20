//! A document a test keeps away from the workspace's service: not held, as far as the service
//! knows, and asked for without being sent until the test lets it go, when it is there as one
//! that just arrived. A part on its way is what a restarted server, a slow network or a large
//! chain gives a request to wait for; a test holds one back to see what the service does
//! meanwhile.
//!
//! Held back [`listed`](HeldBack::listed), the workspace names it among its documents and has
//! still to send it. Held back [`unlisted`](HeldBack::unlisted), the service was told nothing of
//! it, as a member the workspace sends its documents only once they are opened: a part its
//! app wants is asked for, and on its way until it is let go.

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

/// One document held back.
struct Kept {
    /// Whether the workspace's listing names it.
    listed: bool,
    /// Whether the service looked for it since it was held back.
    looked_for: bool,
}

#[derive(Default)]
struct Held {
    /// The documents held back, by workspace and id.
    documents: HashMap<(String, String), Kept>,
    /// How many times a document was held back or let go: what the service listed of its
    /// workspace's documents is listed again once this moved.
    moved: u64,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

fn with<T>(change: impl FnOnce(&mut Held) -> T) -> T {
    change(HELD.lock_recover().get_or_insert_with(Held::default))
}

/// A document of a workspace held back from its service until this is dropped.
#[must_use = "the document is held back only while this is kept"]
pub struct HeldBack {
    document: (String, String),
}

impl HeldBack {
    /// Hold `document` of `workspace` (a chain part's id, as its app names it) back, listed
    /// by the workspace.
    pub fn listed(workspace: &str, document: &str) -> Self {
        Self::hold(workspace, document, true)
    }

    /// Hold `document` of `workspace` back, the service told nothing of it.
    pub fn unlisted(workspace: &str, document: &str) -> Self {
        Self::hold(workspace, document, false)
    }

    fn hold(workspace: &str, document: &str, listed: bool) -> Self {
        let document = (workspace.to_owned(), document.to_owned());
        with(|held| {
            held.documents.insert(
                document.clone(),
                Kept {
                    listed,
                    looked_for: false,
                },
            );
            held.moved = held.moved.wrapping_add(1);
        });
        Self { document }
    }

    /// Whether the service looked for it since it was held back.
    #[must_use]
    pub fn looked_for(&self) -> bool {
        with(|held| {
            held.documents
                .get(&self.document)
                .is_some_and(|kept| kept.looked_for)
        })
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
pub(crate) fn hidden(workspace: &str, document: &str) -> bool {
    with(|held| {
        held.documents
            .get_mut(&(workspace.to_owned(), document.to_owned()))
            .map(|kept| kept.looked_for = true)
            .is_some()
    })
}

/// Whether `document` is held back unlisted: the workspace's listing does not name it.
pub(crate) fn unlisted(workspace: &str, document: &str) -> bool {
    with(|held| {
        held.documents
            .get(&(workspace.to_owned(), document.to_owned()))
            .is_some_and(|kept| !kept.listed)
    })
}

/// How many times a document was held back or let go.
pub(crate) fn moved() -> u64 {
    with(|held| held.moved)
}
