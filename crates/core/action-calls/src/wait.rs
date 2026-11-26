//! Answers that wait for an invocation to move.
//!
//! A caller that wants to know when an invocation finishes used to ask
//! again on a timer: an `Observe` every 200 ms, each a round trip. A waiting
//! request (`wait_ms` on [`RemoteActionMessage::Observe`] or
//! [`InvokeRequest`]) is instead held by the provider until the invocation moves
//! past what the caller has seen, settles, or the wait runs out — so a
//! result reaches its caller one round trip after the provider records it.
//!
//! Nothing here sleeps or holds a thread. A wait parks on the provider's own
//! change signal ([`ActionProvider::next_change`]) and looks again only when it
//! fires; a provider that has none answers at once. The state
//! a waiting answer carries is read when it is answered, as the caller is
//! then, so a caller who lost access in the meantime hears `None`.
//!
//! [`RemoteActionMessage::Observe`]: tilo_actions::remote::RemoteActionMessage::Observe
//! [`InvokeRequest`]: tilo_actions::remote::InvokeRequest

use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionProvider, Caller, Completer, InvocationReceipt, InvocationState, Pending, PendingPoll,
    ReceiptStatus, UpdateMark, pending,
};
use tilo_clock::time::Instant;

/// How often one wait looks again because the provider's change signal had
/// already fired, before it answers with what it has rather than spin.
const MAX_IMMEDIATE_WAKES: u32 = 64;

/// The state of `invocation`, as [`ActionProvider::observe`] reports it to `caller`, once it has
/// moved past `seen` — or, with no `seen`, once it settles, or once a followed read holds its
/// answer ([`InvocationState::read_answered`]) — or at `until`, whichever comes first. A caller
/// following what it has seen hears the state as it stands at `until` too: no wait outlives the
/// caller that asked it. A caller across a connection gives up on an answer that does not come and
/// asks again, and a wait that only a move could end would answer that move once more for every ask
/// it outlived, to an asker that takes each answer for its latest ask.
///
/// It answers at once when the invocation is already there, when `caller`
/// may not see it (`None`), when the provider cannot signal changes, or when
/// `caller`'s execution has ended. Otherwise it waits on
/// [`ActionProvider::next_change`] and looks again each time that fires; the
/// deadline is checked then, so a provider's bounded change interval bounds how
/// far past `until` an answer can come.
pub fn settle_with<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    invocation: InvocationId,
    seen: Option<UpdateMark>,
    until: Instant,
) -> Pending<Option<InvocationState>> {
    let (answer, settled) = pending();
    look(Wait {
        provider: provider.clone(),
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
pub(crate) fn settle_receipt<H: ActionProvider + Clone + Send + 'static>(
    provider: &H,
    caller: &Caller,
    receipt: Pending<InvocationReceipt>,
    until: Instant,
) -> Pending<InvocationReceipt> {
    let provider = provider.clone();
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
        settle_with(&provider, &caller, receipt.invocation, None, until).then(move |state| {
            // Nothing visible to this caller any more: the admission stands,
            // and the next observe says what became of it.
            completer.complete(state.flatten().map_or(receipt, |state| state.receipt()));
        });
    });
    settled
}

/// One parked wait.
struct Wait<H> {
    provider: H,
    caller: Caller,
    invocation: InvocationId,
    seen: Option<UpdateMark>,
    until: Instant,
    answer: Completer<Option<InvocationState>>,
}

impl<H: ActionProvider> Wait<H> {
    /// Whether `state` is worth answering with now.
    fn arrived(&self, state: Option<&InvocationState>) -> bool {
        state.is_none_or(|state| {
            state.is_terminal()
                // A followed read never ends: its first whole answer is what a caller that saw
                // nothing yet waits for.
                || (self.seen.is_none() && state.read_answered)
                || self
                    .seen
                    .is_some_and(|seen| UpdateMark::of(&state.update) != seen)
        })
    }

    /// Whether to stop waiting whatever the state says: at `until`, or once its
    /// caller's execution has ended.
    fn over(&self) -> bool {
        tilo_clock::now() >= self.until
            || self
                .caller
                .execution_lease()
                .is_some_and(|lease| !lease.is_live())
    }
}

fn look<H: ActionProvider + Send + 'static>(wait: Wait<H>) {
    let mut wakes = 0_u32;
    loop {
        if wait.answer.is_abandoned() {
            return;
        }
        // Asked for before looking, so a change between the two still wakes
        // the wait rather than slipping past it.
        let change = wait.provider.next_change(&wait.caller, &wait.invocation);
        let state = wait.provider.observe(&wait.caller, &wait.invocation);
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
