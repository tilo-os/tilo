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
//!
//! A press also says where in its window it was ([`Presses::find`]): a question the call it
//! made raises stands there, whether or not the press proved the call.

use std::collections::VecDeque;
use std::sync::Mutex;
use tilo_clock::time::{Duration, Instant};

use tilo_actions::remote::asked::PressPoint;
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

/// A press of the person's in `window`, issued serial `serial` at `at`, where in the window it
/// was `point`.
#[derive(Clone, Copy, Debug)]
struct Press {
    window: u64,
    serial: u32,
    point: Option<PressPoint>,
    at: Instant,
    /// It proved a call already, and proves no other.
    used: bool,
}

/// What [`Presses::look`] found in its window: no press of that serial at all, or the answer
/// `answer` made of the one there.
enum Looked<T> {
    /// The compositor attested no such press in that window.
    NotThere,
    /// The press is there, and `answer` answered this of it.
    Answered(Option<T>),
}

/// The person's presses the compositor attested, each proving one call.
#[derive(Debug, Default)]
pub(super) struct Presses {
    seen: Mutex<VecDeque<Press>>,
    /// Wakes the call waiting for its press's attestation.
    attested: Notify,
    /// The person's own calls a press on the shell's own surfaces proved, by invocation and the
    /// person, and when: what the app the call is delivered to runs as their act.
    proved: Mutex<VecDeque<(tilo_actions::InvocationId, String, Instant)>>,
}

impl Presses {
    /// The person pressed in `window` at `point` in it, delivered with the input serial
    /// `serial`.
    pub(super) fn note(&self, window: u64, serial: u32, point: Option<PressPoint>, at: Instant) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        seen.retain(|press| at.saturating_duration_since(press.at) <= MAX_PRESS_AGE);
        seen.push_back(Press {
            window,
            serial,
            point,
            at,
            used: false,
        });
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
        self.wait_for(window, serial, |press| {
            if press.used {
                return None;
            }
            press.used = true;
            Some(())
        })
        .await
        .is_some()
    }

    /// Where in `window` the person's press issued serial `serial` was, while it is kept,
    /// whether or not it proved a call: `Some(None)` for a press at no point (a key), `None`
    /// where the compositor attested no such press in that window. Waited for as
    /// [`Self::take`] waits.
    pub(super) async fn find(
        &self,
        window: Option<u64>,
        serial: Option<u32>,
    ) -> Option<Option<PressPoint>> {
        self.wait_for(window, serial, |press| Some(press.point))
            .await
    }

    /// What `answer` makes of the press in `window` issued `serial`, once it is attested, or
    /// `None` when it answers nothing within [`ATTEST_WAIT`].
    async fn wait_for<T>(
        &self,
        window: Option<u64>,
        serial: Option<u32>,
        mut answer: impl FnMut(&mut Press) -> Option<T>,
    ) -> Option<T> {
        let (window, serial) = (window?, serial?);
        let now = tilo_clock::now();
        let deadline = now.checked_add(ATTEST_WAIT).unwrap_or(now);
        loop {
            // The wake is registered before the look, so an attestation that lands between the
            // two is not missed.
            let attested = self.attested.notified();
            if let Looked::Answered(found) =
                self.look(window, serial, tilo_clock::now(), &mut answer)
            {
                return found;
            }
            if tilo_clock::now() >= deadline {
                return None;
            }
            let _ = tokio::time::timeout(
                deadline.saturating_duration_since(tilo_clock::now()),
                attested,
            )
            .await;
        }
    }

    /// The press of `serial` on the shell's own surfaces proved `person`'s call `invocation`, when
    /// the compositor attested it there, fresh and unused, which this uses up.
    async fn prove_shell_call(
        &self,
        (invocation, person): (tilo_actions::InvocationId, String),
        serial: Option<u32>,
    ) {
        if !self
            .take(Some(runtime_collab::wire::shell::SHELL_SURFACES), serial)
            .await
        {
            return;
        }
        let Ok(mut proved) = self.proved.lock() else {
            return;
        };
        let now = tilo_clock::now();
        proved.retain(|(_, _, at)| now.saturating_duration_since(*at) <= MAX_PRESS_AGE);
        proved.push_back((invocation, person, now));
        while proved.len() > KEPT {
            let _ = proved.pop_front();
        }
    }

    /// Whether `person`'s press on the shell's own surfaces proved their call `invocation`,
    /// within [`MAX_PRESS_AGE`].
    fn proved(&self, invocation: tilo_actions::InvocationId, person: &str) -> bool {
        let Ok(proved) = self.proved.lock() else {
            return false;
        };
        let now = tilo_clock::now();
        proved.iter().any(|(proved, by, at)| {
            *proved == invocation
                && by == person
                && now.saturating_duration_since(*at) <= MAX_PRESS_AGE
        })
    }

    /// `answer` of the press, if it is there, without waiting: [`Looked::NotThere`] where it is
    /// not.
    fn look<T>(
        &self,
        window: u64,
        serial: u32,
        now: Instant,
        answer: &mut impl FnMut(&mut Press) -> Option<T>,
    ) -> Looked<T> {
        let Ok(mut seen) = self.seen.lock() else {
            return Looked::Answered(None);
        };
        seen.retain(|press| now.saturating_duration_since(press.at) <= MAX_PRESS_AGE);
        match seen
            .iter_mut()
            .find(|press| press.window == window && press.serial == serial)
        {
            Some(press) => Looked::Answered(answer(press)),
            None => Looked::NotThere,
        }
    }
}

impl super::AppBrokerState {
    /// The person pressed in `window` on their desktop at `point` in it, as the compositor that
    /// saw the input attested it over the shell link: `serial` is the input serial it issued to
    /// that window with the press.
    pub(crate) fn note_press(&self, window: u64, serial: u32, point: Option<PressPoint>) {
        self.presses.note(window, serial, point, tilo_clock::now());
    }

    /// A press older than the window it would prove, for a test that makes one.
    #[cfg(test)]
    pub(crate) fn note_stale_press(&self, window: u64, serial: u32) {
        let stale = tilo_clock::now()
            .checked_sub(MAX_PRESS_AGE)
            .and_then(|at| at.checked_sub(Duration::from_millis(1)));
        if let Some(stale) = stale {
            self.presses.note(window, serial, None, stale);
        }
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

    /// The own call `invocation` of the person whose root key is `person`, made on their shell's
    /// link, names `serial`: the compositor's serial of the press on the shell's own surfaces that
    /// made it, when it names one. Proved once that press is attested there, fresh and unused,
    /// the call runs as their act ([`Self::pressed_call`]); otherwise it is their plain call.
    pub(super) async fn shell_pressed(
        &self,
        (invocation, person): (tilo_actions::InvocationId, [u8; 32]),
        serial: Option<u32>,
    ) {
        self.presses
            .prove_shell_call((invocation, super::person_id_for_root(person)), serial)
            .await;
    }

    /// Whether the press of the person `person` (their member id) on the shell's own surfaces
    /// made their call `invocation` ([`Self::shell_pressed`]).
    pub(super) fn pressed_call(
        &self,
        invocation: tilo_actions::InvocationId,
        person: &str,
    ) -> bool {
        self.presses.proved(invocation, person)
    }

    /// The window connection `peer_id` is and where in it the person's press issued `serial`
    /// was, when the compositor attested that press to that window: where a question the call
    /// naming it raises stands. `None` when the connection names no window, the call names no
    /// serial, or the serial was issued to no press in that window.
    pub(super) async fn pressed_in_window(
        &self,
        peer_id: u64,
        serial: Option<u32>,
    ) -> Option<(u64, Option<PressPoint>)> {
        let window = self.window_of_peer(peer_id)?;
        let point = self.presses.find(Some(window), serial).await?;
        Some((window, point))
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
        presses.note(7, 11, None, at(start, 0));
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
    async fn a_press_says_where_it_was_after_it_proved_its_call() {
        let presses = Presses::default();
        let point = PressPoint { x: 120, y: 48 };
        presses.note(7, 11, Some(point), tilo_clock::now());
        assert_eq!(
            presses.find(Some(8), Some(11)).await,
            None,
            "another window's"
        );
        assert!(presses.take(Some(7), Some(11)).await);
        assert_eq!(
            presses.find(Some(7), Some(11)).await,
            Some(Some(point)),
            "the question its call raises stands where it was"
        );
        assert!(!presses.take(Some(7), Some(11)).await, "still one call");
    }

    #[tokio::test]
    async fn a_press_the_shell_link_still_carries_is_waited_for() {
        let presses = std::sync::Arc::new(Presses::default());
        let behind = std::sync::Arc::clone(&presses);
        let start = tilo_clock::now();
        let noting = tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(10));
            behind.note(5, 33, None, start);
        });
        assert!(
            presses.take(Some(5), Some(33)).await,
            "the attestation arrived while the call waited"
        );
        noting.await.expect("the note lands");
        assert!(!presses.take(Some(5), Some(33)).await, "used once");
    }

    #[tokio::test]
    async fn a_press_on_the_shells_own_surfaces_proves_one_call_of_its_person() {
        use runtime_collab::wire::shell::SHELL_SURFACES;
        let presses = Presses::default();
        let (pick, again, unpressed) = (
            tilo_actions::InvocationId::new(),
            tilo_actions::InvocationId::new(),
            tilo_actions::InvocationId::new(),
        );
        let ada = || "ada".to_owned();
        presses.note(SHELL_SURFACES, 40, None, tilo_clock::now());
        presses.note(7, 41, None, tilo_clock::now());
        presses.prove_shell_call((unpressed, ada()), None).await;
        presses.prove_shell_call((unpressed, ada()), Some(41)).await;
        assert!(
            !presses.proved(unpressed, "ada"),
            "no serial, or one an app's window was issued, proves the shell's call"
        );
        presses.prove_shell_call((pick, ada()), Some(40)).await;
        assert!(presses.proved(pick, "ada"), "the palette's pick is her act");
        assert!(
            !presses.proved(pick, "bo"),
            "it is nobody else's act, whatever call names the invocation"
        );
        presses.prove_shell_call((again, ada()), Some(40)).await;
        assert!(!presses.proved(again, "ada"), "one press proves one call");
        assert!(
            !presses.take(Some(7), Some(40)).await,
            "no app's window takes a press on the shell's surfaces"
        );
    }

    #[tokio::test]
    async fn a_press_older_than_the_window_proves_nothing() {
        let presses = Presses::default();
        let start = tilo_clock::now();
        let stale = start
            .checked_sub(MAX_PRESS_AGE)
            .and_then(|at| at.checked_sub(Duration::from_millis(1)))
            .expect("the test's clock is past its press window");
        presses.note(7, 21, None, stale);
        assert!(!presses.take(Some(7), Some(21)).await);
    }
}
