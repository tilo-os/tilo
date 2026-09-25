//! Client side of PIPELINED_POLL_V1 (`tilo_relay::delivery_v3::poll_chain`):
//! several held event polls outstanding as one ordered chain, so a page no
//! longer waits a full round trip for the next poll to reach the host.
//!
//! Pages are admitted strictly in chain order. Any failed or out-of-order
//! answer abandons the chain; answers of an abandoned chain are ignored (the
//! host may have served them, but nothing after a missing page is admitted),
//! and the next poll starts a new chain at the client's own cursor.
use std::collections::VecDeque;
use tilo_relay::delivery_v3::poll_chain::ChainPoll;

/// Polls kept outstanding while pages keep arriving. One poll when idle, so an
/// idle consumer holds one request at the host like a plain poll.
pub(crate) const CHAIN_DEPTH: usize = 3;

#[derive(Default)]
pub(crate) struct PollChain {
    active: Option<Active>,
    /// The last answered poll carried a page: keep the full depth outstanding.
    busy: bool,
    /// Identity of the previous chain; a new chain never reuses it.
    last: Option<u64>,
}
struct Active {
    id: u64,
    next: u32,
    outstanding: VecDeque<u32>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The oldest outstanding poll of the current chain: admit in order.
    Accept,
    /// Another chain, or out of order: ignore the answer.
    Stale,
}
impl PollChain {
    /// Outstanding polls of the current chain.
    pub fn outstanding(&self) -> usize {
        self.active
            .as_ref()
            .map_or(0, |active| active.outstanding.len())
    }
    /// Whether another poll is wanted now (without allocating a position).
    pub fn wants(&self) -> bool {
        match &self.active {
            None => true,
            Some(active) => active.outstanding.len() < self.depth(),
        }
    }
    fn depth(&self) -> usize {
        if self.busy { CHAIN_DEPTH } else { 1 }
    }
    /// Allocate the next poll position, starting a chain with `fresh` when
    /// none is active. `None` when the chain already has enough polls.
    pub fn next(&mut self, fresh: impl FnOnce() -> u64) -> Option<ChainPoll> {
        if !self.wants() {
            return None;
        }
        match &mut self.active {
            Some(active) => {
                let index = active.next;
                let Some(next) = index.checked_add(1) else {
                    // Exhausted: restart at the client's own cursor.
                    self.active = None;
                    return self.next(fresh);
                };
                active.next = next;
                active.outstanding.push_back(index);
                Some(ChainPoll {
                    chain: active.id,
                    index,
                })
            }
            None => {
                let mut id = fresh();
                if self.last == Some(id) {
                    id = id.wrapping_add(1);
                }
                self.last = Some(id);
                self.active = Some(Active {
                    id,
                    next: 1,
                    outstanding: VecDeque::from([0]),
                });
                Some(ChainPoll {
                    chain: id,
                    index: 0,
                })
            }
        }
    }
    /// A poll was answered (`page`: non-empty body).
    pub fn answered(&mut self, poll: ChainPoll, page: bool) -> Answer {
        let Some(active) = &mut self.active else {
            return Answer::Stale;
        };
        if active.id != poll.chain {
            return Answer::Stale;
        }
        if active.outstanding.front() != Some(&poll.index) {
            // The host answers a chain in order; anything else means an
            // earlier answer went missing. Nothing after it is admissible.
            self.abandon();
            return Answer::Stale;
        }
        active.outstanding.pop_front();
        self.busy = page;
        Answer::Accept
    }
    /// A poll produced no answer (lost reply, timeout, refusal).
    pub fn failed(&mut self, poll: ChainPoll) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.id == poll.chain)
        {
            self.abandon();
        }
    }
    /// Stop the chain (reset, snapshot, stopped carrier, failure).
    pub fn abandon(&mut self) {
        self.active = None;
        self.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids() -> impl FnMut() -> u64 {
        let mut n = 0x10;
        move || {
            n += 1;
            n
        }
    }
    #[test]
    fn idle_chain_holds_one_poll_and_busy_chain_fills_the_depth() {
        let mut fresh = ids();
        let mut chain = PollChain::default();
        let first = chain.next(&mut fresh).unwrap();
        assert_eq!(
            first,
            ChainPoll {
                chain: 0x11,
                index: 0
            }
        );
        // Idle: one outstanding poll, like a plain poll.
        assert!(chain.next(&mut fresh).is_none());
        assert_eq!(chain.answered(first, false), Answer::Accept);
        let second = chain.next(&mut fresh).unwrap();
        assert_eq!(second.index, 1);
        // A page arrived: the chain keeps CHAIN_DEPTH polls outstanding.
        assert_eq!(chain.answered(second, true), Answer::Accept);
        let polls: Vec<_> = std::iter::from_fn(|| chain.next(&mut fresh)).collect();
        assert_eq!(polls.iter().map(|p| p.index).collect::<Vec<_>>(), [2, 3, 4]);
        assert!(polls.iter().all(|p| p.chain == 0x11));
        assert_eq!(chain.outstanding(), CHAIN_DEPTH);
        for poll in &polls {
            assert_eq!(chain.answered(*poll, true), Answer::Accept);
        }
    }
    #[test]
    fn out_of_order_or_failed_answers_abandon_and_ignore_the_rest_of_the_chain() {
        let mut fresh = ids();
        let mut chain = PollChain::default();
        let a = chain.next(&mut fresh).unwrap();
        assert_eq!(chain.answered(a, true), Answer::Accept);
        let (b, c, d) = (
            chain.next(&mut fresh).unwrap(),
            chain.next(&mut fresh).unwrap(),
            chain.next(&mut fresh).unwrap(),
        );
        // c answered before b: b's page may be missing, so nothing is admitted.
        assert_eq!(chain.answered(c, true), Answer::Stale);
        assert_eq!(chain.answered(b, true), Answer::Stale);
        assert_eq!(chain.answered(d, true), Answer::Stale);
        // A new chain restarts at index 0 with a new identity.
        let e = chain.next(&mut fresh).unwrap();
        assert_eq!((e.chain, e.index), (0x12, 0));
        assert_eq!(chain.answered(e, true), Answer::Accept);
        let f = chain.next(&mut fresh).unwrap();
        let g = chain.next(&mut fresh).unwrap();
        // A lost reply abandons: the later answer is ignored.
        chain.failed(f);
        assert_eq!(chain.answered(g, true), Answer::Stale);
        // A failure of an already abandoned chain changes nothing.
        let h = chain.next(&mut fresh).unwrap();
        chain.failed(f);
        assert_eq!(chain.answered(h, false), Answer::Accept);
    }
    #[test]
    fn abandon_resets_depth_and_identity() {
        let mut fresh = ids();
        let mut chain = PollChain::default();
        let a = chain.next(&mut fresh).unwrap();
        chain.answered(a, true);
        chain.next(&mut fresh).unwrap();
        chain.abandon();
        assert_eq!(chain.outstanding(), 0);
        let b = chain.next(&mut fresh).unwrap();
        assert_eq!(b.index, 0);
        assert!(chain.next(&mut fresh).is_none(), "idle depth after abandon");
    }
}
