//! The person's presses, as the compositor that saw the input attested them, and the answer a
//! call an app makes gets about one.
//!
//! The compositor is the only party that sees a person's input reach a window, so it is the one
//! that issues the serial travelling with that press and tells the daemon here
//! ([`runtime_collab::wire::shell::ShellCall::Press`]). An app cannot mint one: a call names a
//! serial, and it runs as the person only when that serial was issued to the window the app's
//! connection is, within [`MAX_PRESS_AGE`], and has not proved another call.
//!
//! The press is the person's whose shell link attested it: only their shell's link can send one,
//! and the app connections the broker admits are that same desktop's. The attestation and the
//! call it proves travel the shell's and the app's own connections, so a call may arrive first:
//! [`Presses::take`] waits [`ATTEST_WAIT`] for a serial it does not know yet.

use std::collections::VecDeque;
use std::sync::Mutex;
use tilo_clock::time::{Duration, Instant};

use tokio::sync::Notify;

/// How long a press proves a call: mainstream launch latency fits inside it, and an old serial
/// stops being a standing permission to act as the person.
pub const MAX_PRESS_AGE: Duration = Duration::from_secs(10);

/// How long a call that names a serial the daemon has not heard yet waits for its attestation:
/// the compositor sends it the moment it delivered the press, beside the call it made possible.
const ATTEST_WAIT: Duration = Duration::from_millis(100);

/// How many presses are kept: enough for every window the person can press within
/// [`MAX_PRESS_AGE`].
const KEPT: usize = 64;

/// A press of the person's in `window`, issued serial `serial` at `at`.
#[derive(Clone, Copy, Debug)]
struct Press {
    window: u64,
    serial: u32,
    at: Instant,
}

/// The person's presses the compositor attested, each proving one call.
#[derive(Debug, Default)]
pub(super) struct Presses {
    seen: Mutex<VecDeque<Press>>,
    /// Wakes the call waiting for its press's attestation.
    attested: Notify,
}

impl Presses {
    /// The person pressed in `window`, delivered with the input serial `serial`.
    pub(super) fn note(&self, window: u64, serial: u32, at: Instant) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        seen.retain(|press| at.saturating_duration_since(press.at) <= MAX_PRESS_AGE);
        seen.push_back(Press { window, serial, at });
        while seen.len() > KEPT {
            let _ = seen.pop_front();
        }
        drop(seen);
        self.attested.notify_waiters();
    }

    /// Whether the person's press in `window`, issued serial `serial`, is behind a call now:
    /// attested within [`MAX_PRESS_AGE`], and not used for another call, which this uses it up.
    /// A serial the compositor issued to another window, or to nobody, proves nothing; neither
    /// does one older than the window or already taken. A serial not attested yet is waited for
    /// [`ATTEST_WAIT`], because the shell link may still be carrying it.
    pub(super) async fn take(&self, window: Option<u64>, serial: Option<u32>) -> bool {
        let (Some(window), Some(serial)) = (window, serial) else {
            return false;
        };
        let deadline = tilo_clock::now() + ATTEST_WAIT;
        loop {
            // The wake is registered before the look, so an attestation that lands between the
            // two is not missed.
            let attested = self.attested.notified();
            if self.consume(window, serial, tilo_clock::now()) {
                return true;
            }
            if tilo_clock::now() >= deadline {
                return false;
            }
            let _ = tokio::time::timeout(
                deadline.saturating_duration_since(tilo_clock::now()),
                attested,
            )
            .await;
        }
    }

    /// Take the press if it is there, without waiting.
    fn consume(&self, window: u64, serial: u32, now: Instant) -> bool {
        let Ok(mut seen) = self.seen.lock() else {
            return false;
        };
        seen.retain(|press| now.saturating_duration_since(press.at) <= MAX_PRESS_AGE);
        let before = seen.len();
        seen.retain(|press| !(press.window == window && press.serial == serial));
        seen.len() != before
    }
}

impl super::AppBrokerState {
    /// The person pressed in `window` on their desktop, as the compositor that saw the input
    /// attested it over the shell link: `serial` is the input serial it issued to that window
    /// with the press.
    pub(crate) fn note_press(&self, window: u64, serial: u32) {
        self.presses.note(window, serial, tilo_clock::now());
    }

    /// A press older than the window it would prove, for a test that makes one.
    #[cfg(test)]
    pub(crate) fn note_stale_press(&self, window: u64, serial: u32) {
        self.presses.note(
            window,
            serial,
            tilo_clock::now() - MAX_PRESS_AGE - Duration::from_millis(1),
        );
    }

    /// Whether the request of connection `peer_id` that names `serial` is the person's own press:
    /// issued to the window that connection is, within [`MAX_PRESS_AGE`], and not used for
    /// another call, which this uses it up. `None` when the connection names no window, or the
    /// call names no serial: neither followed a press.
    pub(super) async fn person_pressed(&self, peer_id: u64, serial: Option<u32>) -> bool {
        self.presses
            .take(self.window_of_peer(peer_id), serial)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start
            .checked_add(Duration::from_millis(ms))
            .expect("a near instant")
    }

    #[tokio::test]
    async fn a_press_proves_one_call_in_its_window_while_it_is_fresh() {
        let presses = Presses::default();
        let start = tilo_clock::now();
        presses.note(7, 11, at(start, 0));
        assert!(
            !presses.take(Some(8), Some(11)).await,
            "a press in another window proves nothing"
        );
        assert!(
            !presses.take(Some(7), Some(12)).await,
            "a serial the compositor did not issue to it proves nothing"
        );
        assert!(
            presses.take(Some(7), Some(11)).await,
            "the window it was issued to"
        );
        assert!(
            !presses.take(Some(7), Some(11)).await,
            "one press proves one call"
        );
        assert!(
            !presses.take(Some(7), None).await,
            "a call that names no serial followed no press"
        );
    }

    #[tokio::test]
    async fn a_press_the_shell_link_still_carries_is_waited_for() {
        let presses = std::sync::Arc::new(Presses::default());
        let behind = std::sync::Arc::clone(&presses);
        let start = tilo_clock::now();
        let noting = tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(10));
            behind.note(5, 33, start);
        });
        assert!(
            presses.take(Some(5), Some(33)).await,
            "the attestation arrived while the call waited"
        );
        noting.await.expect("the note lands");
        assert!(!presses.take(Some(5), Some(33)).await, "used once");
    }

    #[tokio::test]
    async fn a_press_older_than_the_window_proves_nothing() {
        let presses = Presses::default();
        let start = tilo_clock::now();
        presses.note(7, 21, start - MAX_PRESS_AGE - Duration::from_millis(1));
        assert!(!presses.take(Some(7), Some(21)).await);
    }
}
