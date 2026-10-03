//! Lazy, paged access to a channel's op history.
//!
//! A member who joins a channel receives the current state (the latest
//! snapshot) and the key material for everything older, not the history
//! itself: history can be gigabytes and most of it is never looked at.
//! Older ops stay with the hosts that retain them (brokers, relays) and are
//! fetched on demand, a page at a time, newest first — the same
//! `before_seq` scrollback request brokers already serve.
//!
//! [`HistoryPager`] is the client side of that exchange for one artifact:
//! it remembers how far back history is loaded, issues at most one request
//! at a time, prefetches when a view comes within a margin of the loaded
//! edge, and keeps paging when the caller needs a specific older point
//! (opening an old version, searching backwards). It is transport- and
//! crypto-agnostic; callers decrypt pages with keys they already hold.

use crate::channel::ArtifactId;
use crate::op::SignedOp;

/// Collect up to `max` ops of `artifact` with `seq < before_seq`: the newest
/// such window, in ascending `seq` order, plus whether nothing older remains.
/// The page semantics every history host shares.
pub fn page_before<'a>(
    ops: impl IntoIterator<Item = &'a SignedOp>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u16,
) -> (Vec<SignedOp>, bool) {
    let mut matching: Vec<&SignedOp> = ops
        .into_iter()
        .filter(|op| op.artifact == artifact && op.seq < before_seq)
        .collect();
    matching.sort_by_key(|op| op.seq);
    let total = matching.len();
    if max == 0 {
        return (Vec::new(), total == 0);
    }
    let start = total.saturating_sub(usize::from(max));
    let page: Vec<SignedOp> = matching
        .get(start..)
        .unwrap_or_default()
        .iter()
        .map(|op| (*op).clone())
        .collect();
    let exhausted = page.len() >= total;
    (page, exhausted)
}

/// One page request: ops strictly older than `before_seq`, at most `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    /// The artifact whose history is paged.
    pub artifact: ArtifactId,
    /// Exclusive upper bound on `seq`.
    pub before_seq: u64,
    /// Page size.
    pub max: u16,
}

/// Client-side paging state for one artifact's history.
#[derive(Clone, Debug)]
pub struct HistoryPager {
    artifact: ArtifactId,
    page_size: u16,
    prefetch_margin: usize,
    /// Everything with `seq >= oldest_loaded` is loaded; `None` before the
    /// first page or anchor.
    oldest_loaded: Option<u64>,
    /// An outstanding request's `before_seq`.
    in_flight: Option<u64>,
    /// A point the caller asked to reach (older version, search).
    target: Option<u64>,
    exhausted: bool,
}

impl HistoryPager {
    /// A pager fetching `page_size` ops per request and prefetching when a
    /// view is within `prefetch_margin` loaded ops of the oldest one.
    pub fn new(artifact: ArtifactId, page_size: u16, prefetch_margin: usize) -> Self {
        Self {
            artifact,
            page_size: page_size.max(1),
            prefetch_margin,
            oldest_loaded: None,
            in_flight: None,
            target: None,
            exhausted: false,
        }
    }

    /// The artifact this pager fetches.
    pub fn artifact(&self) -> ArtifactId {
        self.artifact
    }

    /// Anchor at the current state: everything from `seq` on arrived live
    /// or in the joining snapshot; history before it is fetched lazily.
    pub fn anchor(&mut self, seq: u64) {
        self.oldest_loaded = Some(self.oldest_loaded.map_or(seq, |oldest| oldest.min(seq)));
    }

    /// Oldest loaded `seq`, if any history is loaded.
    pub fn oldest_loaded(&self) -> Option<u64> {
        self.oldest_loaded
    }

    /// Whether the host reported that nothing older exists.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Whether a request is outstanding.
    pub fn is_loading(&self) -> bool {
        self.in_flight.is_some()
    }

    fn next_request(&mut self) -> Option<PageRequest> {
        if self.exhausted || self.in_flight.is_some() {
            return None;
        }
        let before_seq = self.oldest_loaded.unwrap_or(u64::MAX);
        if before_seq == 0 {
            self.exhausted = true;
            return None;
        }
        self.in_flight = Some(before_seq);
        Some(PageRequest {
            artifact: self.artifact,
            before_seq,
            max: self.page_size,
        })
    }

    /// A history view reports how many loaded ops lie above (older than)
    /// its viewport. Returns a request when the view is close enough to
    /// the loaded edge to prefetch the next page.
    pub fn on_viewport(&mut self, loaded_ops_above_viewport: usize) -> Option<PageRequest> {
        if loaded_ops_above_viewport > self.prefetch_margin {
            return None;
        }
        self.next_request()
    }

    /// The caller needs history back to `seq` (opening an older version,
    /// searching backwards). Returns the next request, and keeps requesting
    /// after each page until `seq` is loaded or history is exhausted.
    pub fn ensure_loaded_back_to(&mut self, seq: u64) -> Option<PageRequest> {
        if self.oldest_loaded.is_some_and(|oldest| oldest <= seq) {
            return None;
        }
        self.target = Some(self.target.map_or(seq, |target| target.min(seq)));
        self.next_request()
    }

    /// Record a page of ops answering the outstanding request; see
    /// [`Self::on_page`].
    pub fn on_page_ops(&mut self, page: &[SignedOp], exhausted: bool) -> Option<PageRequest> {
        let oldest = page
            .iter()
            .filter(|op| op.artifact == self.artifact)
            .map(|op| op.seq)
            .min();
        self.on_page(oldest, exhausted)
    }

    /// Record the answer to the outstanding request: the oldest `seq` the
    /// host returned (before the caller dropped anything it could not use
    /// yet, such as an incomplete segment) and whether nothing older exists.
    /// Returns the next request when an [`Self::ensure_loaded_back_to`]
    /// target is still not reached. An answer to no outstanding request is
    /// ignored.
    pub fn on_page(&mut self, oldest_seq: Option<u64>, exhausted: bool) -> Option<PageRequest> {
        let before_seq = self.in_flight.take()?;
        let oldest_in_page = oldest_seq.filter(|seq| *seq < before_seq);
        if let Some(seq) = oldest_in_page {
            self.oldest_loaded = Some(seq);
        }
        if exhausted {
            if oldest_in_page.is_none() && before_seq != u64::MAX {
                self.oldest_loaded = Some(before_seq);
            }
            self.exhausted = true;
            self.target = None;
            return None;
        }
        // Nothing yet, but more exists: the host is fetching it from its
        // peers. Keep the position; the next trigger asks again.
        oldest_in_page?;
        match self.target {
            Some(target) if self.oldest_loaded.is_some_and(|oldest| oldest > target) => {
                self.next_request()
            }
            _ => {
                self.target = None;
                None
            }
        }
    }

    /// Forget an outstanding request that will never be answered (transport
    /// reset), so the next trigger re-issues it.
    pub fn reset_in_flight(&mut self) {
        self.in_flight = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Ed25519PublicKey;
    use crate::identity::AttestationId;
    use crate::role::OpClass;

    fn op(artifact: ArtifactId, seq: u64) -> SignedOp {
        SignedOp::new_unsigned(
            seq.to_le_bytes().to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes([1; 32]),
            AttestationId::new(),
            OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    /// Scrolling back prefetches near the edge and stops when exhausted; opening an old version pages
    /// until it is loaded; unsolicited or foreign pages change nothing; an empty non-final page is
    /// asked again.
    #[test]
    fn history_paging_fetches_only_what_the_view_needs() {
        // Phase: scrolling_back_prefetches_near_the_edge_and_stops_when_exhausted.
        {
            let artifact = ArtifactId::new();
            let log: Vec<SignedOp> = (1..=250).map(|seq| op(artifact, seq)).collect();
            let mut pager = HistoryPager::new(artifact, 100, 20);
            pager.anchor(240);
            assert_eq!(pager.on_viewport(50), None, "far from the edge");
            let request = pager.on_viewport(10).unwrap();
            assert_eq!(request.before_seq, 240);
            assert_eq!(pager.on_viewport(0), None, "one request at a time");
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            assert_eq!(page.len(), 100);
            assert_eq!(page.first().unwrap().seq, 140);
            assert_eq!(pager.on_page_ops(&page, exhausted), None);
            assert_eq!(pager.oldest_loaded(), Some(140));

            let request = pager.on_viewport(5).unwrap();
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            pager.on_page_ops(&page, exhausted);
            let request = pager.on_viewport(5).unwrap();
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            assert!(exhausted);
            pager.on_page_ops(&page, exhausted);
            assert_eq!(pager.oldest_loaded(), Some(1));
            assert!(pager.is_exhausted());
            assert_eq!(pager.on_viewport(0), None);
        }

        // Phase: opening_an_old_version_pages_until_it_is_loaded.
        {
            let artifact = ArtifactId::new();
            let log: Vec<SignedOp> = (1..=1000).map(|seq| op(artifact, seq)).collect();
            let mut pager = HistoryPager::new(artifact, 64, 8);
            pager.anchor(1001);
            let mut request = pager.ensure_loaded_back_to(700);
            let mut fetched = 0;
            while let Some(next) = request {
                fetched += 1;
                let (page, exhausted) = page_before(&log, artifact, next.before_seq, next.max);
                request = pager.on_page_ops(&page, exhausted);
            }
            assert_eq!(fetched, 5, "only the pages covering the requested range");
            assert!(pager.oldest_loaded().unwrap() <= 700);
            assert!(!pager.is_exhausted());
            assert_eq!(pager.ensure_loaded_back_to(800), None, "already loaded");
        }

        // Phase: unsolicited_or_foreign_pages_change_nothing.
        {
            let artifact = ArtifactId::new();
            let mut pager = HistoryPager::new(artifact, 10, 0);
            pager.anchor(50);
            assert_eq!(pager.on_page_ops(&[op(artifact, 3)], false), None);
            assert_eq!(pager.oldest_loaded(), Some(50));
            let request = pager.on_viewport(0).unwrap();
            assert_eq!(request.before_seq, 50);
            assert_eq!(pager.on_page_ops(&[op(ArtifactId::new(), 3)], false), None);
            assert!(!pager.is_exhausted());
            assert_eq!(pager.oldest_loaded(), Some(50));
        }

        // Phase: an_empty_page_that_is_not_final_is_asked_again.
        {
            let artifact = ArtifactId::new();
            let mut pager = HistoryPager::new(artifact, 10, 0);
            pager.anchor(50);
            let first = pager.ensure_loaded_back_to(10).unwrap();
            // The host had nothing locally and asked its peers.
            assert_eq!(pager.on_page(None, false), None);
            assert!(!pager.is_exhausted());
            let again = pager.ensure_loaded_back_to(10).unwrap();
            assert_eq!(again.before_seq, first.before_seq);
            assert_eq!(pager.on_page(Some(40), false).unwrap().before_seq, 40);
            assert_eq!(pager.on_page(None, true), None);
            assert!(pager.is_exhausted());
            assert_eq!(pager.oldest_loaded(), Some(40));
        }
    }
}
