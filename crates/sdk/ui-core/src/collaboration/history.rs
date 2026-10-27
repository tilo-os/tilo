//! On-demand document history for collaboration routes.
//!
//! Joining a route delivers the current document (a snapshot) and, on
//! encrypted routes, the key material for all earlier content; the op
//! history itself stays with the broker and relays. [`DocumentHistory`]
//! pages it in when something needs it — a history view scrolling towards
//! its oldest loaded entry, opening an older version, searching backwards —
//! using [`collab_types::history::HistoryPager`] per artifact, and turns each
//! decision into a [`CollabRequest::FetchScrollback`]. Pages arrive as
//! [`super::CollabEvent::ScrollbackBatch`] and are decrypted at the endpoint
//! with keys it already holds, so no other member needs to be online.

use std::collections::HashMap;

use collab_types::ArtifactId;
use collab_types::history::{HistoryPager, PageRequest};

use super::CollabRequest;

/// Ops per history page. Large enough that a scroll gesture rarely waits,
/// small enough to keep each page's decrypt and apply well under a frame.
pub const HISTORY_PAGE_SIZE: u16 = 50;
/// Prefetch when a view is within this many loaded ops of the oldest one.
pub const HISTORY_PREFETCH_MARGIN: usize = 25;

/// Lazy history paging state for every artifact the app has seen.
#[derive(Debug, Default)]
pub struct DocumentHistory {
    pagers: HashMap<ArtifactId, HistoryPager>,
    /// Oldest `seq` delivered live or in a snapshot, per artifact: where
    /// paging back starts.
    oldest_live: HashMap<ArtifactId, u64>,
}

fn fetch(request: PageRequest) -> CollabRequest {
    CollabRequest::FetchScrollback {
        artifact: request.artifact,
        before_seq: request.before_seq,
        max: request.max,
    }
}

impl DocumentHistory {
    /// Note an op that arrived live (or as the joining snapshot).
    pub fn observe_live(&mut self, artifact: ArtifactId, seq: u64) {
        let oldest = self.oldest_live.entry(artifact).or_insert(seq);
        *oldest = (*oldest).min(seq);
        if let Some(pager) = self.pagers.get_mut(&artifact) {
            pager.anchor(seq);
        }
    }

    fn pager(&mut self, artifact: ArtifactId) -> &mut HistoryPager {
        let oldest_live = self.oldest_live.get(&artifact).copied();
        self.pagers.entry(artifact).or_insert_with(|| {
            let mut pager = HistoryPager::new(artifact, HISTORY_PAGE_SIZE, HISTORY_PREFETCH_MARGIN);
            if let Some(seq) = oldest_live {
                pager.anchor(seq);
            }
            pager
        })
    }

    /// A history view reports how many loaded ops lie above its viewport;
    /// returns the fetch to send when it nears the loaded edge.
    pub fn on_viewport(
        &mut self,
        artifact: ArtifactId,
        loaded_ops_above_viewport: usize,
    ) -> Option<CollabRequest> {
        self.pager(artifact)
            .on_viewport(loaded_ops_above_viewport)
            .map(fetch)
    }

    /// Something needs history back to `seq` (an older version, a search
    /// hit); returns the first fetch. Later pages follow from
    /// [`Self::on_batch`] until `seq` is loaded.
    pub fn ensure_loaded_back_to(
        &mut self,
        artifact: ArtifactId,
        seq: u64,
    ) -> Option<CollabRequest> {
        self.pager(artifact).ensure_loaded_back_to(seq).map(fetch)
    }

    /// Record a scrollback batch; returns the follow-up fetch, if a target
    /// is not reached yet.
    pub fn on_batch(
        &mut self,
        artifact: ArtifactId,
        oldest_seq: Option<u64>,
        exhausted: bool,
    ) -> Option<CollabRequest> {
        self.pagers
            .get_mut(&artifact)?
            .on_page(oldest_seq, exhausted)
            .map(fetch)
    }

    /// Paging state for `artifact`, if any history was requested.
    pub fn pager_for(&self, artifact: ArtifactId) -> Option<&HistoryPager> {
        self.pagers.get(&artifact)
    }

    /// Forget outstanding requests after a transport reset.
    pub fn reset_in_flight(&mut self) {
        for pager in self.pagers.values_mut() {
            pager.reset_in_flight();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn before_seq(request: Option<CollabRequest>) -> Option<u64> {
        match request {
            Some(CollabRequest::FetchScrollback { before_seq, .. }) => Some(before_seq),
            _ => None,
        }
    }

    #[test]
    fn history_pages_backwards_from_the_oldest_live_op_until_loaded() {
        {
            let artifact = ArtifactId::new();
            let mut history = DocumentHistory::default();
            history.observe_live(artifact, 5_000);
            history.observe_live(artifact, 5_001);
            assert_eq!(before_seq(history.on_viewport(artifact, 100)), None);
            assert_eq!(before_seq(history.on_viewport(artifact, 3)), Some(5_000));
            assert_eq!(
                before_seq(history.on_viewport(artifact, 0)),
                None,
                "in flight"
            );
            assert_eq!(
                before_seq(history.on_batch(artifact, Some(4_950), false)),
                None
            );
            assert_eq!(before_seq(history.on_viewport(artifact, 10)), Some(4_950));
            history.on_batch(artifact, Some(4_900), true);
            assert!(history.pager_for(artifact).unwrap().is_exhausted());
            assert_eq!(before_seq(history.on_viewport(artifact, 0)), None);
        }

        {
            let artifact = ArtifactId::new();
            let mut history = DocumentHistory::default();
            history.observe_live(artifact, 1_000);
            assert_eq!(
                before_seq(history.ensure_loaded_back_to(artifact, 880)),
                Some(1_000)
            );
            assert_eq!(
                before_seq(history.on_batch(artifact, Some(950), false)),
                Some(950)
            );
            assert_eq!(
                before_seq(history.on_batch(artifact, Some(900), false)),
                Some(900)
            );
            assert_eq!(
                before_seq(history.on_batch(artifact, Some(850), false)),
                None
            );
            assert_eq!(
                history.ensure_loaded_back_to(artifact, 870).map(|_| ()),
                None
            );
        }
    }
}
