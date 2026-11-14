//! Presence on its way to one connection: batched, in posting order.
//!
//! Every workspace a connection is subscribed to posts the presence it fans out
//! into the connection's [`PresenceMailbox`] instead of its frame queue, and
//! the connection's writer takes what is due as one batch per workspace.
//!
//! - **In order.** Presence is delivered in the order it was posted: a
//!   caret never overtakes the pointer before it, and nothing is written
//!   for a workspace after its `Unsubscribe`.
//! - **Batched, for at most [`BATCH_WINDOW`].** Presence waits a few
//!   milliseconds so a keystroke's frames travel in one packet, and goes at
//!   once as soon as it is due.
//! - **Presence rides along.** Presence that may go early goes with
//!   whatever the connection writes next ([`PresenceMailbox::take`] with
//!   `ride`), so a delta and the presence beside it share a packet.
//! - **Bounded, newest kept.** A connection that does not drain its
//!   presence loses the oldest pending state, not the one posted now, so a
//!   pointer that is dragged fast still comes to rest where it stopped.
//!
//! Each payload is shared, not copied, between the connections it is posted
//! to. Deltas never pass through here, so edit delivery never waits for
//! presence.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;
use tokio::sync::Notify;
use tokio::time::Instant;

/// How long presence waits for more to travel in the same frame.
pub const BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(5);
/// Pending presence one connection holds at most — the frame queue it
/// replaces held as many. Presence can be large (a shared window's video
/// frame), so a slow reader drops it rather than falling behind.
const MAX_PENDING: usize = 32;

/// One presence payload the writer is to send now.
#[derive(Debug)]
pub struct DuePresence {
    /// The workspace it was fanned out in.
    pub workspace_id: WorkspaceId,
    /// The author-stamped `AttributedPresence` bytes, shared by every
    /// connection it was posted to.
    pub payload: Arc<[u8]>,
}

/// The presence pending for one connection.
#[derive(Default)]
pub struct PresenceMailbox {
    state: Mutex<State>,
    notify: Notify,
}

#[derive(Default)]
struct State {
    events: VecDeque<Event>,
    /// The deadline the writer is waiting for, if it is waiting.
    armed: Option<Instant>,
}

struct Event {
    workspace_id: WorkspaceId,
    payload: Arc<[u8]>,
    since: Instant,
}

/// `at + by`, or `at` itself past the end of time.
fn later(at: Instant, by: std::time::Duration) -> Instant {
    at.checked_add(by).unwrap_or(at)
}

impl State {
    fn due_of(event: &Event) -> Instant {
        later(event.since, BATCH_WINDOW)
    }

    fn next_due(&self) -> Option<Instant> {
        self.events.front().map(Self::due_of)
    }
}

impl PresenceMailbox {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    /// Post `payload`, which its author sent to workspace `workspace_id`. A full
    /// mailbox drops its oldest pending presence to take this one.
    pub fn post(&self, workspace_id: WorkspaceId, payload: Arc<[u8]>, now: Instant) {
        let mut state = self.state();
        if state.events.len() >= MAX_PENDING {
            state.events.pop_front();
        }
        state.events.push_back(Event {
            workspace_id,
            payload,
            since: now,
        });
        let wake = state
            .next_due()
            .is_some_and(|due| state.armed.is_none_or(|armed| due < armed));
        drop(state);
        if wake {
            self.notify.notify_one();
        }
    }

    /// When the earliest pending presence falls due, if any is pending.
    pub fn next_due(&self) -> Option<Instant> {
        self.state().next_due()
    }

    /// Wait until some presence is due. Cancel-safe: nothing is taken.
    pub async fn ready(&self) {
        loop {
            let next = {
                let mut state = self.state();
                let next = state.next_due();
                state.armed = next;
                next
            };
            match next {
                Some(at) if at <= Instant::now() => return,
                Some(at) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(at) => return,
                        () = self.notify.notified() => {}
                    }
                }
                None => self.notify.notified().await,
            }
        }
    }

    /// Take everything due at `now`, in posting order; with `ride`, also
    /// everything pending because something is being written to the
    /// connection anyway.
    pub fn take(&self, now: Instant, ride: bool) -> Vec<DuePresence> {
        let mut taken = Vec::new();
        let mut state = self.state();
        state.armed = None;
        while let Some(event) = state.events.front() {
            if !ride && State::due_of(event) > now {
                break;
            }
            let Some(event) = state.events.pop_front() else {
                break;
            };
            taken.push(event);
        }
        drop(state);
        taken
            .into_iter()
            .map(|event| DuePresence {
                workspace_id: event.workspace_id,
                payload: event.payload,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tilo_relay_protocol::WorkspaceId {
        tilo_relay_protocol::WorkspaceId::from_u128(7)
    }

    fn bytes(text: &str) -> Arc<[u8]> {
        Arc::from(text.as_bytes().to_vec())
    }

    fn texts(due: &[DuePresence]) -> Vec<String> {
        due.iter()
            .map(|presence| String::from_utf8_lossy(&presence.payload).into_owned())
            .collect()
    }

    #[test]
    fn presence_goes_after_the_window_in_posting_order() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(workspace(), bytes("pointer"), t0);
        mailbox.post(workspace(), bytes("caret"), t0);
        assert_eq!(mailbox.next_due(), Some(t0 + BATCH_WINDOW));
        assert!(
            mailbox.take(t0, false).is_empty(),
            "the batch window is open"
        );
        assert_eq!(
            texts(&mailbox.take(t0 + BATCH_WINDOW, false)),
            ["pointer", "caret"]
        );
        assert_eq!(mailbox.next_due(), None);
    }

    #[test]
    fn riding_takes_presence_before_its_window() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(workspace(), bytes("caret"), t0);
        assert!(mailbox.take(t0, false).is_empty());
        assert_eq!(texts(&mailbox.take(t0, true)), ["caret"]);
        assert_eq!(mailbox.next_due(), None);
    }

    #[test]
    fn a_full_mailbox_keeps_the_newest_presence() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        for step in 0..MAX_PENDING {
            mailbox.post(workspace(), bytes(&step.to_string()), t0);
        }
        mailbox.post(workspace(), bytes("last"), t0);
        let due = mailbox.take(t0 + BATCH_WINDOW, false);
        assert_eq!(due.len(), MAX_PENDING);
        assert_eq!(texts(&due).last().map(String::as_str), Some("last"));
        assert_eq!(
            texts(&due).first().map(String::as_str),
            Some("1"),
            "the oldest pending state went, not the newest"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_writer_wakes_when_presence_falls_due() {
        let mailbox = Arc::new(PresenceMailbox::default());
        let waiter = tokio::spawn({
            let mailbox = mailbox.clone();
            async move {
                mailbox.ready().await;
                Instant::now()
            }
        });
        tokio::task::yield_now().await;
        let posted = Instant::now();
        mailbox.post(workspace(), bytes("caret"), posted);
        let woke = waiter.await.unwrap();
        assert!(woke >= posted + BATCH_WINDOW);
        assert_eq!(texts(&mailbox.take(woke, false)), ["caret"]);
    }
}
