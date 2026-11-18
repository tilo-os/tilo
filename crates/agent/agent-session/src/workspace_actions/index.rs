//! What search answers from, kept as the workspaces' documents land.
//!
//! A find ranks what each document's app publishes to the search index
//! ([`ui::search::index_publication::IndexEntry`]). Reading that from the
//! replica opens the document, and a document is expensive to open: a
//! workspace of a hundred notes took seconds to read whole, which is what a
//! person waited for on their first find and again whenever a write had
//! landed. So the service reads each document's entries between requests, in
//! short passes, as the document lands or as the workspace joins, and a find
//! answers from what is kept without opening anything.

use super::{Duration, Instant, Ordering, Worker, WorkspaceDocuments, WorkspaceId};

/// How much of one pass the index build keeps the thread from the requests queued behind it:
/// short enough that a request never waits on the pass, long enough that a workspace of a
/// hundred documents is indexed within a few seconds of joining.
pub(super) const WARM_SLICE: Duration = Duration::from_millis(15);

impl Worker {
    /// Read what search finds in the documents the workspaces hold, for `budget` at most: each
    /// document's app tells what it publishes to the search index, read here as it lands or
    /// joins, so a find answers from the index and never opens one while the person waits.
    /// `true` when there is nothing more to read now: every document is in the index, or what
    /// is left could not be read yet, which the next request looks at again.
    pub(super) fn warm_entries(&mut self, budget: Duration) -> bool {
        let began = Instant::now();
        let mut titles = std::mem::take(&mut self.titles);
        let ids: Vec<WorkspaceId> = self.workspaces.keys().copied().collect();
        let mut all = true;
        let mut read_any = false;
        'workspaces: for id in ids {
            let waiting = self
                .workspaces
                .get(&id)
                .and_then(|workspace| workspace.documents.as_ref())
                .map(|kept| kept.unindexed(16))
                .unwrap_or_default();
            for route in waiting {
                let read = |bytes: Vec<u8>| titles.entries(&route.app_id, bytes);
                let Some(workspace) = self.workspaces.get_mut(&id) else {
                    break 'workspaces;
                };
                let Some(kept) = workspace.documents.as_mut() else {
                    break 'workspaces;
                };
                let (_, opened) = kept.entries(&route, read);
                if opened {
                    read_any = true;
                    self.shared.search.warmed.fetch_add(1, Ordering::Relaxed);
                }
                if began.elapsed() >= budget {
                    all = false;
                    break 'workspaces;
                }
            }
        }
        self.titles = titles;
        // A pass that read nothing leaves documents it cannot read yet: the next request looks
        // again rather than this thread spinning on them.
        all || !read_any
    }

    /// Whether any workspace holds a document search keeps no entries for: what a pass of
    /// [`Self::warm_entries`] reads next.
    pub(super) fn unindexed_any(&self) -> bool {
        self.workspaces
            .values()
            .filter_map(|workspace| workspace.documents.as_ref())
            .any(WorkspaceDocuments::has_unindexed)
    }
}
