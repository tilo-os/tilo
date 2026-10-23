//! The search index a library builds off the window's thread: the worker
//! owns all O(n) index construction, and the window installs the finished
//! projection.

use std::sync::{Arc, mpsc};

use crate::model::{Annotation, Book};

/// A completed immutable search projection. The worker owns all O(n) index
/// construction; the UI thread only installs this already-complete value.
pub(super) struct SearchBuild {
    pub(super) fingerprint: u64,
    pub(super) generation: u64,
    pub(super) provider: crate::search_provider::BookSearch,
}

pub(super) struct SearchWorker {
    results: mpsc::Receiver<SearchBuild>,
}

impl SearchWorker {
    /// `prior` is the published provider; its chapter-prose index is reused
    /// when the book set has not changed.
    pub(super) fn spawn(
        prior: crate::search_provider::BookSearch,
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<crate::model::LibraryEntry>>,
        fingerprint: u64,
        generation: u64,
    ) -> Result<Self, String> {
        let (send, results) = mpsc::sync_channel(1);
        ui::rt::spawn_thread("ebook-search-builder", move || {
            let provider = crate::search_provider::BookSearch::new_shared_after(
                &prior,
                books,
                annotations,
                registry,
                generation,
            );
            // The receiver is intentionally allowed to disappear during
            // shutdown; publication has no side effect outside the app.
            let _ = send.send(SearchBuild {
                fingerprint,
                generation,
                provider,
            });
        })
        .map_err(|error| error.to_string())?;
        Ok(Self { results })
    }

    pub(super) fn try_recv(&self) -> Option<SearchBuild> {
        self.results.try_recv().ok()
    }
}
