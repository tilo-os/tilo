//! Answers that wait for an invocation to move.
//!
//! A caller that wants to know when an invocation finishes used to ask
//! again on a timer: an `Observe` every 200 ms, each a round trip. A waiting
//! request (`wait_ms` on [`RemoteActionMessage::Observe`] or
//! [`InvokeRequest`]) is instead held by the host until the invocation moves
//! past what the caller has seen, settles, or the wait runs out — so a
//! result reaches its caller one round trip after the host records it.
//!
//! Nothing here sleeps or holds a thread. A wait parks on the host's own
//! change signal ([`ActionHost::next_change`]) and looks again only when it
//! fires; a host that has none answers at once, as it always did. The state
//! a waiting answer carries is read when it is answered, as the caller is
//! then, so a caller who lost access in the meantime hears `None`.
//!
//! [`RemoteActionMessage::Observe`]: super::RemoteActionMessage::Observe
//! [`InvokeRequest`]: super::InvokeRequest

use serde::{Deserialize, Serialize};
use tilo_clock::{Duration, Instant};

use super::{
    ActionHost, Caller, Completer, InvocationReceipt, InvocationState, Pending, PendingPoll,
    ReceiptStatus, pending,
};
use crate::outcome::{InvocationId, InvocationUpdate};

/// The longest a host holds a waiting answer, whatever `wait_ms` asked, so a
/// relay connection or a broker call is never held open indefinitely.
pub const MAX_REMOTE_WAIT: Duration = Duration::from_secs(10);

/// How often one wait looks again because the host's change signal had
/// already fired, before it answers with what it has rather than spin.
const MAX_IMMEDIATE_WAKES: u32 = 64;

/// How far an invocation's updates had got when its caller last looked:
/// what a waiting [`Observe`](super::RemoteActionMessage::Observe) asks the
/// host to move past.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpdateMark {
    Accepted,
    Progress {
        sequence: u64,
    },
    Item {
        sequence: u64,
    },
    /// Settled: nothing follows.
    Finished,
}

impl UpdateMark {
    /// Where `update` stands.
    #[must_use]
    pub const fn of(update: &InvocationUpdate) -> Self {
        match update {
            InvocationUpdate::Accepted { .. } => Self::Accepted,
            InvocationUpdate::Progress { sequence, .. } => Self::Progress {
                sequence: *sequence,
            },
            InvocationUpdate::Item { sequence, .. } => Self::Item {
                sequence: *sequence,
            },
            InvocationUpdate::Succeeded { .. }
            | InvocationUpdate::Failed { .. }
            | InvocationUpdate::Cancelled { .. } => Self::Finished,
        }
    }
}

/// When a wait of `wait_ms` asked now ends, capped at [`MAX_REMOTE_WAIT`].
pub(super) fn deadline(wait_ms: u32) -> Instant {
    let now = tilo_clock::now();
    let wait = Duration::from_millis(u64::from(wait_ms)).min(MAX_REMOTE_WAIT);
    now.checked_add(wait).unwrap_or(now)
}

/// The state of `invocation`, as [`ActionHost::observe`] reports it to
/// `caller`, once it has moved past `seen` — or, with no `seen`, once it
/// settles — or at `until`, whichever comes first.
///
/// It answers at once when the invocation is already there, when `caller`
/// may not see it (`None`), when the host cannot signal changes, or when
/// `caller`'s execution has ended. Otherwise it waits on
/// [`ActionHost::next_change`] and looks again each time that fires; the
/// deadline is checked then, so a host's bounded change interval bounds how
/// far past `until` an answer can come.
pub fn settle_with<H: ActionHost + Clone + Send + 'static>(
    host: &H,
    caller: &Caller,
    invocation: InvocationId,
    seen: Option<UpdateMark>,
    until: Instant,
) -> Pending<Option<InvocationState>> {
    let (answer, settled) = pending();
    look(Wait {
        host: host.clone(),
        caller: caller.clone(),
        invocation,
        seen,
        until,
        answer,
    });
    settled
}

/// `receipt`, held while it says the invocation is still running until it
/// settles or `until` passes — what a waiting `Invoke` answers with.
pub(super) fn settle_receipt<H: ActionHost + Clone + Send + 'static>(
    host: &H,
    caller: &Caller,
    receipt: Pending<InvocationReceipt>,
    until: Instant,
) -> Pending<InvocationReceipt> {
    let host = host.clone();
    let caller = caller.clone();
    let (completer, settled) = pending();
    receipt.then(move |receipt| {
        // An abandoned admission abandons this answer too: dropping the
        // completer is how `serve` hears it.
        let Some(receipt) = receipt else {
            return;
        };
        if !matches!(receipt.status, ReceiptStatus::Accepted) {
            completer.complete(receipt);
            return;
        }
        settle_with(&host, &caller, receipt.invocation, None, until).then(move |state| {
            // Nothing visible to this caller any more: the admission stands,
            // and the next observe says what became of it.
            completer.complete(state.flatten().map_or(receipt, |state| state.receipt()));
        });
    });
    settled
}

/// One parked wait.
struct Wait<H> {
    host: H,
    caller: Caller,
    invocation: InvocationId,
    seen: Option<UpdateMark>,
    until: Instant,
    answer: Completer<Option<InvocationState>>,
}

impl<H: ActionHost> Wait<H> {
    /// Whether `state` is worth answering with now.
    fn arrived(&self, state: Option<&InvocationState>) -> bool {
        state.is_none_or(|state| {
            state.is_terminal()
                || self
                    .seen
                    .is_some_and(|seen| UpdateMark::of(&state.update) != seen)
        })
    }

    /// Whether to stop waiting whatever the state says.
    fn over(&self) -> bool {
        tilo_clock::now() >= self.until
            || self
                .caller
                .execution_lease()
                .is_some_and(|lease| !lease.is_live())
    }
}

fn look<H: ActionHost + Send + 'static>(wait: Wait<H>) {
    let mut wakes = 0_u32;
    loop {
        if wait.answer.is_abandoned() {
            return;
        }
        // Asked for before looking, so a change between the two still wakes
        // the wait rather than slipping past it.
        let change = wait.host.next_change(&wait.caller, &wait.invocation);
        let state = wait.host.observe(&wait.caller, &wait.invocation);
        let answer_now =
            wait.arrived(state.as_ref()) || wait.over() || wakes >= MAX_IMMEDIATE_WAKES;
        let Some(mut change) = change.filter(|_| !answer_now) else {
            wait.answer.complete(state);
            return;
        };
        if matches!(change.poll_ready(), PendingPoll::Waiting) {
            // Whether it fires or its producer goes away, look again.
            change.then(move |_| look(wait));
            return;
        }
        wakes = wakes.saturating_add(1);
    }
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
