//! Following an invocation: one ask held until it moves, not a poll.
//!
//! The API's and an agent's writes are followed by asking how the invocation
//! stands. Answered at once, every ask costs a round trip and the server's
//! per-frame membership read, so a caller asked again and again for as long
//! as its write took to save. An ask that waits is held here instead, on the
//! invocation log's own moves, and answered once the invocation moved or its
//! wait ended.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    Caller, InvocationState, MAX_REMOTE_WAIT, Pending, RemoteActionMessage, UpdateMark, pending,
};
use tilo_lock::MutexRecover as _;

use super::{Shared, WorkspaceActions};

/// How long a held follow looks again at the log even when it was not woken:
/// short, so an asker that goes away frees the thread, and never the source of
/// a wait's latency (the log wakes it the moment the invocation moves).
const FOLLOW_LOOK: Duration = Duration::from_millis(100);

impl WorkspaceActions {
    /// The state of invocation `id` as `caller` reads it now, or — when the
    /// ask waits and the invocation has not moved past `seen` — once it moves
    /// or the wait ends: held here rather than answered at once, so a caller
    /// following a write asks once instead of on a timer.
    pub(super) fn follow(
        &self,
        caller: &Caller,
        correlation: u64,
        invocation: InvocationId,
        seen: Option<UpdateMark>,
        wait_ms: Option<u32>,
    ) -> Pending<RemoteActionMessage> {
        let read = |shared: &Shared| {
            shared
                .log
                .lock_recover()
                .owned(caller, &invocation)
                .map(|entry| {
                    InvocationState::new(invocation, entry.update.clone())
                        .left_at(entry.left_at.clone())
                        .linked(entry.link.clone())
                        .with_untrusted(super::tree_put::answers_from_outside(
                            &entry.request.target,
                        ))
                })
        };
        let Some(wait) = wait_ms
            .filter(|wait| *wait > 0)
            .map(|wait| std::time::Duration::from_millis(u64::from(wait)).min(MAX_REMOTE_WAIT))
        else {
            return Pending::ready(RemoteActionMessage::Update {
                invocation,
                state: read(&self.shared),
                correlation: Some(correlation),
            });
        };
        let until = std::time::Instant::now()
            .checked_add(wait)
            .unwrap_or_else(std::time::Instant::now);
        let shared = Arc::clone(&self.shared);
        let caller = caller.clone();
        let (completer, answer) = pending();
        let spawned = std::thread::Builder::new()
            .name("invocation-follow".into())
            .spawn(move || {
                loop {
                    let (generation, state, moves) = {
                        let log = shared.log.lock_recover();
                        (
                            log.generation(),
                            log.owned(&caller, &invocation).map(|entry| {
                                InvocationState::new(invocation, entry.update.clone())
                                    .left_at(entry.left_at.clone())
                                    .linked(entry.link.clone())
                                    .with_untrusted(super::tree_put::answers_from_outside(
                                        &entry.request.target,
                                    ))
                            }),
                            log.moves(),
                        )
                    };
                    let arrived = state.as_ref().is_none_or(|state| {
                        state.is_terminal()
                            || seen.is_some_and(|seen| UpdateMark::of(&state.update) != seen)
                    });
                    let now = std::time::Instant::now();
                    if arrived || now >= until || completer.is_abandoned() {
                        completer.complete(RemoteActionMessage::Update {
                            invocation,
                            state,
                            correlation: Some(correlation),
                        });
                        return;
                    }
                    // Looked again on the log's own move, and at least this
                    // often, so an asker that goes away is noticed.
                    let bound = until.saturating_duration_since(now).min(FOLLOW_LOOK);
                    moves.wait(generation, bound);
                }
            });
        if spawned.is_err() {
            // No thread to hold the answer on: answer with what is known, and
            // the caller asks again.
            return Pending::ready(RemoteActionMessage::Update {
                invocation,
                state: read(&self.shared),
                correlation: Some(correlation),
            });
        }
        answer
    }
}
