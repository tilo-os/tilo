//! Where the large parts of the actions of the documents a session keeps are kept: one store per
//! document, by the app whose document it is, as the host lends each
//! ([`ResourceService::provide_content`](crate::resource_service::ResourceService::provide_content)).
//!
//! An action past the history budget keeps its long text and byte runs as content and records a
//! reference to each ([`tilo_actions::content`]). A part belongs to the document whose action it
//! is: it is kept in that document's store, which the host offers on the document's route, so the
//! document's other readers fetch it from there, and it is read back from whichever of them keeps
//! it — an address is its bytes' own, and a reference is checked against the bytes it opens, so
//! a document's store never answers for a part of another's.
//!
//! A session's documents live in one document, so its reads reach all of them through one store
//! ([`Documents::store`]); a part is still kept in the document the action was made on, whose route
//! carries it.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use tilo_content::{
    Collected, ContentBackend, ContentError, ContentReader, ContentState, ContentStore, Stored,
};

/// The content stores of the documents one service keeps, by the app whose document each is.
#[derive(Default)]
pub struct Records {
    stores: Mutex<BTreeMap<String, ContentStore>>,
    /// The document whose action is being committed now: the one a part is kept in.
    writing: Mutex<Option<String>>,
}

impl Records {
    /// Keep the content of the document of `app` in `store` from now on.
    pub fn keep_in(&self, app: &str, store: ContentStore) {
        Self::lock(&self.stores).insert(app.to_owned(), store);
    }

    /// Keep the parts of the actions of the document of `app` committed from now.
    pub fn writing(&self, app: &str) {
        *Self::lock(&self.writing) = Some(app.to_owned());
    }

    /// The store the documents' code reaches as its own: a part kept now goes to the document being
    /// committed, and a part read comes from the document that names it.
    #[must_use]
    pub fn store(self: &Arc<Self>) -> ContentStore {
        ContentStore::from_backend(Arc::clone(self) as Arc<dyn ContentBackend>)
    }

    fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        held.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn kept(&self) -> Vec<ContentStore> {
        Self::lock(&self.stores).values().cloned().collect()
    }

    /// The store of the document being committed now, when this service was lent one.
    fn committing(&self) -> Option<ContentStore> {
        let app = Self::lock(&self.writing).clone()?;
        Self::lock(&self.stores).get(&app).cloned()
    }
}

impl ContentBackend for Records {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        let store = self.committing().ok_or_else(|| {
            ContentError::Io(
                "no document this service keeps was lent a store to keep its parts in".to_owned(),
            )
        })?;
        store.put_reader(reader, mime)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        let stores = self.kept();
        if let Some(store) = stores.iter().find(|store| store.has(hash)) {
            return Ok(store.open(hash));
        }
        // Not here yet: one document's route is asked for it — the one the service last lent a
        // store to, since which record names it is not what the reference says.
        Ok(stores.last().and_then(|store| store.open(hash)))
    }

    fn has(&self, hash: &str) -> bool {
        self.kept().iter().any(|store| store.has(hash))
    }

    fn state(&self, hash: &str) -> ContentState {
        if self.has(hash) {
            return ContentState::Resolved;
        }
        if Self::lock(&self.stores).is_empty() {
            return ContentState::Unavailable;
        }
        // Opening it asks the document's route for it ([`ContentBackend::open`] of the document's
        // store).
        ContentState::Pending
    }

    fn collect_unreferenced(
        &self,
        _live: &std::collections::BTreeSet<String>,
    ) -> Result<Collected, ContentError> {
        // What each document keeps is that document's, and the host that lent its store governs its
        // retention: this service collects none of it.
        Err(ContentError::CollectionUnsupported)
    }

    fn shared(&self) -> bool {
        self.kept().iter().any(ContentStore::shared)
    }

    fn workspace_scoped(&self) -> bool {
        self.kept().iter().any(ContentStore::workspace_scoped)
    }

    fn max_item_bytes(&self) -> usize {
        tilo_content::ContentLimits::default().max_item_bytes
    }
}
