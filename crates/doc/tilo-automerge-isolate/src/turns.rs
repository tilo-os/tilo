//! Which waiting request a free worker serves next.
//!
//! Every caller of a pool shares its workers: on a server, every workspace
//! it holds. A request waits its turn here rather than on one worker, so
//! that no caller's backlog decides another's wait:
//!
//! - small requests go first, and large ones never hold the last worker
//!   while there is more than one: a few hundred bytes to check is never
//!   stuck behind a whole document's rebuild, however many of those wait;
//! - among requests of one size, the caller served longest ago goes next,
//!   and a caller's own requests go in the order it made them: one caller
//!   restoring many documents takes its turn with the others, not all of
//!   theirs first.
//!
//! A request whose state a worker already holds waits for that worker while
//! a small request goes to it, since rebuilding the state elsewhere costs
//! more than that wait; behind a large one it takes any free worker.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

use tilo_lock::MutexRecover as _;

/// A request that sends the worker at least this many bytes, untrusted
/// input and any state it rebuilds from together, is large: rebuilding a
/// whole document of this size takes far longer than checking a change.
pub(crate) const LARGE_BYTES: usize = 64 * 1024;

/// Served callers remembered beyond those waiting, before the rest are
/// forgotten.
const SERVED: usize = 1024;

/// A request for a worker.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Job {
    pub(crate) caller: u64,
    pub(crate) bytes: usize,
    /// The worker that holds the state the request builds on.
    pub(crate) preferred: Option<usize>,
}

pub(crate) struct Turns {
    state: Mutex<State>,
    granted: Condvar,
}

struct State {
    /// Per worker: whether the request it serves is large, while it serves one.
    running: Vec<Option<bool>>,
    /// Per worker: whether it held a started process when last released.
    live: Vec<bool>,
    waiting: Vec<Waiting>,
    granted: HashMap<u64, usize>,
    /// When each caller was last given a worker.
    served: HashMap<u64, u64>,
    tick: u64,
    tickets: u64,
}

struct Waiting {
    ticket: u64,
    caller: u64,
    large: bool,
    preferred: Option<usize>,
}

impl Turns {
    pub(crate) fn new(workers: usize) -> Self {
        Self {
            state: Mutex::new(State {
                running: vec![None; workers],
                live: vec![false; workers],
                waiting: Vec::new(),
                granted: HashMap::new(),
                served: HashMap::new(),
                tick: 0,
                tickets: 0,
            }),
            granted: Condvar::new(),
        }
    }

    /// Wait for `job`'s turn: the worker it is given, which is its own until
    /// [`Self::release`].
    pub(crate) fn take(&self, job: Job) -> usize {
        let mut state = self.state.lock_recover();
        state.tickets = state.tickets.saturating_add(1);
        let ticket = state.tickets;
        state.waiting.push(Waiting {
            ticket,
            caller: job.caller,
            large: job.bytes >= LARGE_BYTES,
            preferred: job.preferred,
        });
        if state.dispatch() {
            self.granted.notify_all();
        }
        loop {
            if let Some(worker) = state.granted.remove(&ticket) {
                return worker;
            }
            state = self
                .granted
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Give `worker` back, saying whether it holds a started process.
    pub(crate) fn release(&self, worker: usize, live: bool) {
        let mut state = self.state.lock_recover();
        if let Some(running) = state.running.get_mut(worker) {
            *running = None;
        }
        if let Some(held) = state.live.get_mut(worker) {
            *held = live;
        }
        if state.dispatch() {
            self.granted.notify_all();
        }
    }
}

impl State {
    /// Give free workers to waiting requests while any can take one; whether
    /// any was given.
    fn dispatch(&mut self) -> bool {
        let mut any = false;
        while let Some((at, worker)) = self.next() {
            let waiting = self.waiting.swap_remove(at);
            if let Some(running) = self.running.get_mut(worker) {
                *running = Some(waiting.large);
            }
            self.tick = self.tick.saturating_add(1);
            self.served.insert(waiting.caller, self.tick);
            self.granted.insert(waiting.ticket, worker);
            any = true;
        }
        if self.served.len() > SERVED.saturating_add(self.waiting.len()) {
            let waiting: std::collections::HashSet<u64> =
                self.waiting.iter().map(|waiting| waiting.caller).collect();
            self.served.retain(|caller, _| waiting.contains(caller));
        }
        any
    }

    /// The waiting request to serve next, by its place in `waiting`, and the
    /// worker it is given.
    fn next(&self) -> Option<(usize, usize)> {
        let large_running = self
            .running
            .iter()
            .filter(|running| **running == Some(true))
            .count();
        let large_allowed = self.running.len().saturating_sub(1).max(1);
        for large in [false, true] {
            if large && large_running >= large_allowed {
                continue;
            }
            let best = self
                .waiting
                .iter()
                .enumerate()
                .filter(|(_, waiting)| waiting.large == large)
                .filter_map(|(at, waiting)| {
                    let worker = self.worker_for(waiting)?;
                    let served = self.served.get(&waiting.caller).copied().unwrap_or(0);
                    Some(((served, waiting.ticket), at, worker))
                })
                .min_by_key(|(order, _, _)| *order);
            if let Some((_, at, worker)) = best {
                return Some((at, worker));
            }
        }
        None
    }

    fn worker_for(&self, waiting: &Waiting) -> Option<usize> {
        if let Some(preferred) = waiting.preferred {
            match self.running.get(preferred) {
                Some(None) => return Some(preferred),
                Some(Some(false)) => return None,
                Some(Some(true)) | None => {}
            }
        }
        let free = |worker: &usize| self.running.get(*worker) == Some(&None);
        let workers = || 0..self.running.len();
        workers()
            .filter(free)
            .find(|worker| self.live.get(*worker) == Some(&true))
            .or_else(|| workers().find(free))
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test holds the schedule it inspects to its end"
)]
mod tests {
    use super::*;

    const SMALL: usize = 512;
    const LARGE: usize = 4 * 1024 * 1024;

    fn job(caller: u64, bytes: usize) -> Job {
        Job {
            caller,
            bytes,
            preferred: None,
        }
    }

    /// Queue `jobs` behind workers busy with small requests, then serve them
    /// one at a time on worker 0 and say whose each turn was.
    fn order(workers: usize, jobs: &[Job]) -> Vec<u64> {
        let turns = Turns::new(workers);
        let mut state = turns.state.lock_recover();
        state.running = vec![Some(false); workers];
        for (ticket, job) in (1..).zip(jobs) {
            state.waiting.push(Waiting {
                ticket,
                caller: job.caller,
                large: job.bytes >= LARGE_BYTES,
                preferred: job.preferred,
            });
        }
        let callers: HashMap<u64, u64> = (1..).zip(jobs.iter().map(|job| job.caller)).collect();
        let mut served = Vec::new();
        while !state.waiting.is_empty() {
            state.running[0] = None;
            assert!(state.dispatch(), "a free worker was left idle");
            let (&ticket, _) = state.granted.iter().next().unwrap();
            state.granted.remove(&ticket);
            served.push(callers[&ticket]);
        }
        served
    }

    #[test]
    fn callers_take_turns_whatever_their_backlog() {
        let mut jobs: Vec<Job> = (0..6).map(|_| job(1, LARGE)).collect();
        jobs.extend((0..2).map(|_| job(2, LARGE)));
        jobs.push(job(3, LARGE));
        assert_eq!(order(2, &jobs), [1, 2, 3, 1, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn small_requests_go_before_large_ones() {
        let mut jobs: Vec<Job> = (0..4).map(|_| job(1, LARGE)).collect();
        jobs.push(job(1, SMALL));
        jobs.push(job(2, SMALL));
        assert_eq!(order(2, &jobs)[..2], [1, 2]);
        let turns = Turns::new(2);
        let mut state = turns.state.lock_recover();
        state.running = vec![Some(true); 2];
        state.waiting.push(Waiting {
            ticket: 1,
            caller: 1,
            large: true,
            preferred: None,
        });
        state.waiting.push(Waiting {
            ticket: 2,
            caller: 2,
            large: false,
            preferred: None,
        });
        state.running[1] = None;
        assert!(state.dispatch());
        assert_eq!(state.granted.get(&2), Some(&1), "the small request first");
    }

    #[test]
    fn large_requests_leave_a_worker_for_small_ones() {
        let turns = Turns::new(3);
        let mut state = turns.state.lock_recover();
        for ticket in 1..=4 {
            state.waiting.push(Waiting {
                ticket,
                caller: 1,
                large: true,
                preferred: None,
            });
        }
        assert!(state.dispatch());
        assert_eq!(state.granted.len(), 2, "large requests took every worker");
        state.waiting.push(Waiting {
            ticket: 5,
            caller: 2,
            large: false,
            preferred: None,
        });
        assert!(state.dispatch());
        assert!(state.granted.contains_key(&5));

        // With one worker, a large request still goes through.
        let turns = Turns::new(1);
        let mut state = turns.state.lock_recover();
        state.waiting.push(Waiting {
            ticket: 1,
            caller: 1,
            large: true,
            preferred: None,
        });
        assert!(state.dispatch());
    }

    #[test]
    fn a_request_waits_for_the_worker_holding_its_state_only_behind_a_small_one() {
        let turns = Turns::new(2);
        let mut state = turns.state.lock_recover();
        state.running = vec![Some(false), None];
        state.waiting.push(Waiting {
            ticket: 1,
            caller: 1,
            large: false,
            preferred: Some(0),
        });
        assert!(!state.dispatch(), "it left the worker holding its state");
        state.running[0] = Some(true);
        assert!(state.dispatch());
        assert_eq!(state.granted.get(&1), Some(&1));
    }

    #[test]
    fn a_free_worker_with_a_started_process_is_used_first() {
        let turns = Turns::new(3);
        turns.release(2, true);
        assert_eq!(turns.take(job(1, SMALL)), 2);
        assert_eq!(turns.take(job(1, SMALL)), 0);
    }
}
