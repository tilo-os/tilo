//! Contiguous upload progress. FlowCredit never becomes native admission.
pub(crate) struct UploadProgress {
    total: usize,
    offset: usize,
    receipt_probe: bool,
}
impl UploadProgress {
    pub fn new(total: usize) -> Result<Self, String> {
        if total == 0 || total > 64 * 1024 * 1024 {
            return Err("canonical upload exceeds bound".into());
        }
        Ok(Self {
            total,
            offset: 0,
            receipt_probe: false,
        })
    }
    pub fn next(&mut self) -> Result<std::ops::Range<usize>, String> {
        let end = self.offset.saturating_add(240 * 1024).min(self.total);
        if end == self.offset {
            if self.receipt_probe {
                return Err("native admission receipt still unavailable".into());
            }
            self.receipt_probe = true;
            let start = ((self.total - 1) / (240 * 1024)) * (240 * 1024);
            return Ok(start..self.total);
        }
        Ok(self.offset..end)
    }
    pub fn credit(&mut self, next: u64) -> Result<(), String> {
        let next = usize::try_from(next).map_err(|_| "upload offset overflow")?;
        if next > self.total || next <= self.offset {
            return Err("upload progress mismatch or stalled".into());
        }
        self.offset = next;
        Ok(())
    }
}

/// Largest item sent whole inside a multi-item `POST /peer/v3/state`. Larger
/// items use the staged (chunked) upload, one item at a time.
pub(crate) const INLINE_ITEM_BYTES: usize = 240 * 1024;
/// Server bound on `StateItemChunk` frames per state request (`prepare_state`).
pub(crate) const MAX_BATCH_ITEMS: usize = 64;
/// Request body bound shared by the host (1 MiB) and the JS executor, less a
/// margin for frame and chunk headers.
pub(crate) const MAX_BATCH_BYTES: usize = 1024 * 1024 - 16 * 1024;
/// Per-item framing overhead upper bound (frame header + chunk envelope).
pub(crate) const ITEM_OVERHEAD_BYTES: usize = 128;
/// Bounded number of state POSTs in flight. The first change is sent at once;
/// later changes coalesce into the next free slot. The window only fills when
/// the round trip exceeds the change interval (long-RTT links): on a LAN each
/// receipt returns before the next keystroke and one request is in flight.
/// This is a client transport bound; the host admits state requests serially
/// in arrival order and never limited the count per session.
pub(crate) const MAX_STATE_IN_FLIGHT: usize = 4;
/// Byte budget of the resend prefix: a new request repeats the still
/// unadmitted items of earlier in-flight requests (outbox order) ahead of its
/// new items, so it is self-contained even if the host happens to admit it
/// before an earlier request (HTTP/2 streams are dispatched concurrently).
/// A repeated item is answered with its original durable receipt. Above this
/// budget the request carries only new items and reordering falls back to
/// RetryLater + drain. Kept small (about three typing changes): on lossy
/// long-RTT links every extra TCP segment raises the chance of a
/// head-of-line stall, and measured resend bytes cost more than the rare
/// reorder they avoid.
pub(crate) const RESEND_PREFIX_BYTES: usize = 2 * 1024;
/// A reordered item (answered RetryLater while another state request was in
/// flight) is re-sent alone at most this many times before failing.
const MAX_REORDER_RETRIES: u8 = 3;

/// One inline upload item, identified exactly as on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ItemKey {
    pub delivery: [u8; 16],
    pub digest: [u8; 32],
}
/// Per-item result of one state request, in request order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ItemOutcome {
    /// Accepted or Duplicate at native committed scope.
    Admitted,
    /// The host lacks a prerequisite (for example the author's earlier change
    /// that is still in another request).
    RetryLater,
    /// Any other outcome; the existing contract treats it as fatal.
    Refused(String),
}
struct Flight {
    items: Vec<ItemKey>,
    /// Another state request was in flight at some point during this one, so
    /// the host may have observed them in either order.
    concurrent: bool,
}
/// Bounded in-order inline upload pipeline. It only plans transport
/// submissions: admission remains the host's durable receipt, and the local
/// outbox is still retired only by signed native coverage.
#[derive(Default)]
pub(crate) struct UploadWindow {
    next: u64,
    flights: std::collections::BTreeMap<u64, Flight>,
    /// Set after a reordered RetryLater: the next request waits until the
    /// window drains, so the retried item reaches the host after its
    /// predecessors.
    drain: bool,
    retries: std::collections::BTreeMap<[u8; 32], u8>,
}
/// Items admitted by a settled request (mark as submitted).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    pub admitted: Vec<[u8; 32]>,
    pub retry: Vec<[u8; 32]>,
}
impl UploadWindow {
    pub fn is_empty(&self) -> bool {
        self.flights.is_empty()
    }
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        self.flights
            .values()
            .any(|flight| flight.items.iter().any(|item| &item.digest == digest))
    }
    /// Whether a new state request may be issued now.
    pub fn can_send(&self) -> bool {
        self.flights.len() < MAX_STATE_IN_FLIGHT && !(self.drain && !self.flights.is_empty())
    }
    /// Select the next batch from unadmitted outbox candidates in outbox order:
    /// `(delivery, digest, canonical length)`, including items already in
    /// flight. In-flight items form a resend prefix while they fit
    /// [`RESEND_PREFIX_BYTES`]; otherwise they are skipped as before. A batch
    /// always carries at least one item that is not yet in flight. Stops at
    /// the item/byte bound or before an item that needs the staged path.
    /// Returns `None` when nothing new is eligible or no slot is free.
    pub fn plan(
        &mut self,
        candidates: impl IntoIterator<Item = (ItemKey, usize)>,
    ) -> Option<(u64, Vec<ItemKey>)> {
        if !self.can_send() {
            return None;
        }
        let candidates: Vec<(ItemKey, usize)> = candidates.into_iter().collect();
        let resend = candidates
            .iter()
            .filter(|(key, _)| self.contains(&key.digest))
            .try_fold(0_usize, |sum, (_, len)| {
                let sum = sum.checked_add(len.checked_add(ITEM_OVERHEAD_BYTES)?)?;
                (sum <= RESEND_PREFIX_BYTES).then_some(sum)
            })
            .is_some();
        let mut items = Vec::new();
        let mut bytes = 0_usize;
        let mut fresh = 0_usize;
        for (key, len) in candidates {
            if items.iter().any(|i: &ItemKey| i.digest == key.digest) {
                continue;
            }
            let in_flight = self.contains(&key.digest);
            if in_flight && !resend {
                continue;
            }
            if len == 0 || len > INLINE_ITEM_BYTES {
                break;
            }
            let cost = len + ITEM_OVERHEAD_BYTES;
            if items.len() == MAX_BATCH_ITEMS || bytes + cost > MAX_BATCH_BYTES {
                break;
            }
            bytes += cost;
            items.push(key);
            fresh += usize::from(!in_flight);
        }
        if fresh == 0 {
            return None;
        }
        self.drain = false;
        let concurrent = !self.flights.is_empty();
        for flight in self.flights.values_mut() {
            flight.concurrent |= concurrent;
        }
        self.next += 1;
        self.flights.insert(
            self.next,
            Flight {
                items: items.clone(),
                concurrent,
            },
        );
        Some((self.next, items))
    }
    /// Apply one complete response. `outcomes` pairs each response frame's
    /// identity with its result; it must match the request exactly, in order.
    pub fn settle(
        &mut self,
        batch: u64,
        outcomes: Vec<(ItemKey, ItemOutcome)>,
    ) -> Result<Settled, String> {
        let flight = self
            .flights
            .remove(&batch)
            .ok_or("unknown or settled state upload")?;
        if outcomes.len() != flight.items.len()
            || outcomes
                .iter()
                .zip(&flight.items)
                .any(|((got, _), want)| got != want)
        {
            return Err("upload admission identity mismatch".into());
        }
        let mut settled = Settled::default();
        for (key, outcome) in outcomes {
            match outcome {
                ItemOutcome::Admitted => {
                    self.retries.remove(&key.digest);
                    settled.admitted.push(key.digest);
                }
                ItemOutcome::RetryLater if flight.concurrent => {
                    let count = self.retries.entry(key.digest).or_default();
                    *count += 1;
                    if *count > MAX_REORDER_RETRIES {
                        return Err("native upload prerequisite still missing".into());
                    }
                    self.drain = true;
                    settled.retry.push(key.digest);
                }
                ItemOutcome::RetryLater => {
                    return Err("native upload not admitted: prerequisite missing".into());
                }
                ItemOutcome::Refused(reason) => {
                    return Err(format!("native upload not admitted: {reason}"));
                }
            }
        }
        Ok(settled)
    }
    /// The request produced no response (lost reply, timeout, transport
    /// retry). Its items become eligible again; a resend of an already durable
    /// item returns the host's original receipt.
    pub fn abandon(&mut self, batch: u64) {
        self.flights.remove(&batch);
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> ItemKey {
        ItemKey {
            delivery: [n; 16],
            digest: [n; 32],
        }
    }
    fn admitted(keys: &[ItemKey]) -> Vec<(ItemKey, ItemOutcome)> {
        keys.iter().map(|k| (*k, ItemOutcome::Admitted)).collect()
    }
    /// Items large enough that an in-flight prefix exceeds the resend budget.
    const NO_RESEND: usize = RESEND_PREFIX_BYTES;
    #[test]
    fn first_change_goes_alone_then_later_changes_coalesce_in_order() {
        let mut window = UploadWindow::default();
        let (first, items) = window.plan([(key(1), NO_RESEND)]).unwrap();
        assert_eq!(items, vec![key(1)]);
        // Further slots: sent immediately, skipping the (large) in-flight item.
        let (second, items) = window
            .plan([(key(1), NO_RESEND), (key(2), NO_RESEND)])
            .unwrap();
        assert_eq!(items, vec![key(2)]);
        let mut rest = Vec::new();
        for n in 3..(MAX_STATE_IN_FLIGHT as u8 + 1) {
            let (batch, items) = window.plan([(key(n), NO_RESEND)]).unwrap();
            assert_eq!(items, vec![key(n)]);
            rest.push(batch);
        }
        // Window full: later changes wait and then coalesce into one request.
        let next = MAX_STATE_IN_FLIGHT as u8 + 1;
        assert!(window.plan([(key(next), NO_RESEND)]).is_none());
        window.settle(first, admitted(&[key(1)])).unwrap();
        let (_, items) = window
            .plan([(key(2), 10), (key(next), 10), (key(next + 1), 10)])
            .unwrap();
        // The small in-flight item 2 now fits the resend prefix.
        assert_eq!(items, vec![key(2), key(next), key(next + 1)]);
        assert!(window.settle(second, admitted(&[key(2)])).is_ok());
    }
    #[test]
    fn small_in_flight_items_are_resent_ahead_of_new_items() {
        let mut window = UploadWindow::default();
        let (a, items) = window.plan([(key(1), 600)]).unwrap();
        assert_eq!(items, vec![key(1)]);
        let (b, items) = window.plan([(key(1), 600), (key(2), 600)]).unwrap();
        assert_eq!(items, vec![key(1), key(2)]);
        let (c, items) = window
            .plan([(key(1), 600), (key(2), 600), (key(3), 600)])
            .unwrap();
        assert_eq!(items, vec![key(1), key(2), key(3)]);
        // Nothing new: no request only repeats in-flight items.
        assert!(
            window
                .plan([(key(1), 600), (key(2), 600), (key(3), 600)])
                .is_none()
        );
        // The host admitted c first (HTTP/2 dispatch order): self-contained,
        // so every item is admitted and no drain is needed.
        let settled = window
            .settle(c, admitted(&[key(1), key(2), key(3)]))
            .unwrap();
        assert!(settled.retry.is_empty());
        assert!(window.can_send());
        // The earlier requests are answered with the original receipts.
        window.settle(a, admitted(&[key(1)])).unwrap();
        window.settle(b, admitted(&[key(1), key(2)])).unwrap();
        assert!(window.is_empty());
    }
    #[test]
    fn resend_prefix_is_bounded_by_bytes_and_batch_bounds() {
        let mut window = UploadWindow::default();
        let half = RESEND_PREFIX_BYTES / 2;
        window.plan([(key(1), half)]).unwrap();
        // One half-budget item fits the prefix (with framing overhead).
        let (_, items) = window.plan([(key(1), half), (key(2), half)]).unwrap();
        assert_eq!(items, vec![key(1), key(2)]);
        // Two in-flight half-budget items exceed it: only new items are sent.
        let (_, items) = window
            .plan([(key(1), half), (key(2), half), (key(3), 10)])
            .unwrap();
        assert_eq!(items, vec![key(3)]);
        // The prefix counts against the 64-item bound.
        let fits = (RESEND_PREFIX_BYTES / (1 + ITEM_OVERHEAD_BYTES)) as u8;
        let mut window = UploadWindow::default();
        window.plan((0..fits).map(|n| (key(n), 1))).unwrap();
        let (_, items) = window.plan((0..100u8).map(|n| (key(n), 1))).unwrap();
        assert_eq!(items.len(), MAX_BATCH_ITEMS);
        assert_eq!(
            items[..fits as usize],
            (0..fits).map(key).collect::<Vec<_>>()[..]
        );
    }
    #[test]
    fn batch_respects_item_byte_and_staged_bounds() {
        let mut window = UploadWindow::default();
        let many = (0..100u8).map(|n| (key(n), 1));
        let (_, items) = window.plan(many).unwrap();
        assert_eq!(items.len(), MAX_BATCH_ITEMS);
        let mut window = UploadWindow::default();
        let big = (0..10u8).map(|n| (key(n), INLINE_ITEM_BYTES));
        let (_, items) = window.plan(big).unwrap();
        assert_eq!(
            items.len(),
            MAX_BATCH_BYTES / (INLINE_ITEM_BYTES + ITEM_OVERHEAD_BYTES)
        );
        // A staged-size item stops the batch so later items never overtake it.
        let mut window = UploadWindow::default();
        let (_, items) = window
            .plan([(key(1), 5), (key(2), INLINE_ITEM_BYTES + 1), (key(3), 5)])
            .unwrap();
        assert_eq!(items, vec![key(1)]);
        assert!(
            UploadWindow::default()
                .plan([(key(2), INLINE_ITEM_BYTES + 1)])
                .is_none()
        );
    }
    #[test]
    fn partial_acceptance_requeues_only_reordered_items_and_drains_first() {
        let mut window = UploadWindow::default();
        let (a, _) = window.plan([(key(1), NO_RESEND)]).unwrap();
        let (b, _) = window
            .plan([(key(1), NO_RESEND), (key(2), 1), (key(3), 1)])
            .unwrap();
        // The host saw b before a: item 2 lacks its predecessor, item 3 is fine.
        let settled = window
            .settle(
                b,
                vec![
                    (key(2), ItemOutcome::RetryLater),
                    (key(3), ItemOutcome::Admitted),
                ],
            )
            .unwrap();
        assert_eq!(settled.admitted, vec![[3; 32]]);
        assert_eq!(settled.retry, vec![[2; 32]]);
        // Retry waits until every earlier request settled.
        assert!(!window.can_send());
        assert!(window.plan([(key(2), 1)]).is_none());
        window.settle(a, admitted(&[key(1)])).unwrap();
        let (c, items) = window.plan([(key(2), 1)]).unwrap();
        assert_eq!(items, vec![key(2)]);
        // Alone, a repeated RetryLater is a genuine missing prerequisite.
        assert!(
            window
                .settle(c, vec![(key(2), ItemOutcome::RetryLater)])
                .is_err()
        );
    }
    #[test]
    fn lone_retry_later_refusal_and_identity_mismatch_stay_fatal() {
        let mut window = UploadWindow::default();
        let (a, _) = window.plan([(key(1), 1)]).unwrap();
        assert!(
            window
                .settle(a, vec![(key(1), ItemOutcome::RetryLater)])
                .is_err()
        );
        let (b, _) = window.plan([(key(1), 1)]).unwrap();
        assert!(
            window
                .settle(b, vec![(key(1), ItemOutcome::Refused("x".into()))])
                .is_err()
        );
        let (c, _) = window.plan([(key(1), 1), (key(2), 1)]).unwrap();
        assert!(window.settle(c, admitted(&[key(2), key(1)])).is_err());
        let (d, _) = window.plan([(key(1), 1)]).unwrap();
        assert!(window.settle(d, vec![]).is_err());
        assert!(
            window.settle(d, admitted(&[key(1)])).is_err(),
            "settled once"
        );
    }
    #[test]
    fn lost_reply_makes_items_eligible_again_and_retries_are_bounded() {
        let mut window = UploadWindow::default();
        let (a, _) = window.plan([(key(1), 1), (key(2), 1)]).unwrap();
        window.abandon(a);
        assert!(window.is_empty());
        // Resend carries the same items first, plus anything newer.
        let (b, items) = window
            .plan([(key(1), 1), (key(2), 1), (key(3), 1)])
            .unwrap();
        assert_eq!(items, vec![key(1), key(2), key(3)]);
        window.settle(b, admitted(&items)).unwrap();
        // Reorder retries of one item are bounded.
        let mut window = UploadWindow::default();
        for round in 0..=MAX_REORDER_RETRIES {
            let (x, _) = window.plan([(key(9), NO_RESEND)]).unwrap();
            let (y, _) = window.plan([(key(9), NO_RESEND), (key(8), 1)]).unwrap();
            window
                .settle(x, vec![(key(9), ItemOutcome::RetryLater)])
                .map(|_| ())
                .unwrap_or_else(|e| {
                    assert_eq!(round, MAX_REORDER_RETRIES, "{e}");
                });
            window.abandon(y);
            window.drain = false;
        }
    }
    #[test]
    fn complete_staging_credit_recovers_receipt_with_one_exact_final_nonempty_range() {
        let mut value = UploadProgress::new(300 * 1024).unwrap();
        assert_eq!(value.next().unwrap(), 0..240 * 1024);
        value.credit(300 * 1024).unwrap();
        assert_eq!(value.next().unwrap(), 240 * 1024..300 * 1024);
        assert!(
            value.credit(300 * 1024).is_err(),
            "a repeated staging credit is still not native admission"
        );
    }
    #[test]
    fn upload_progress_rejects_oversize_stall_and_keeps_exact_resume_offset() {
        assert!(UploadProgress::new(0).is_err());
        assert!(UploadProgress::new(64 * 1024 * 1024 + 1).is_err());
        let mut value = UploadProgress::new(300 * 1024).unwrap();
        assert!(value.credit(0).is_err());
        assert!(value.credit(300 * 1024 + 1).is_err());
        value.credit(13).unwrap();
        assert_eq!(value.next().unwrap(), 13..13 + 240 * 1024);
    }
}
