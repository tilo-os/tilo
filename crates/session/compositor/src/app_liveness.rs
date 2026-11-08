//! Whether an app still answers: a window whose app stops answering says so,
//! and offers to wait or to quit it.
//!
//! When input reaches an app, the compositor sends its `xdg_wm_base` a ping.
//! A client reads its events in order, so a client that does not answer
//! within [`UNANSWERED`] has not read the input either: it is hung (busy in a
//! loop, deadlocked, stopped). Its window then says "Not responding" with
//! "Wait" and "Force quit". The answer arriving clears that by itself;
//! "Wait" hides it for another [`UNANSWERED`] while the ping stays out;
//! "Force quit" ends the client.
//!
//! Pings go out only on input the app was given, at most one in flight per
//! client, so an app nobody touches is never woken and never judged: a
//! window off screen, whose frame callbacks are held, could not answer from
//! inside a blocked swap and must not be called hung.

use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::backend::ClientId;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::shell::PingError;
use smithay::wayland::shell::xdg::ShellClient;

/// How long a ping may stay unanswered before the app counts as not responding. Long enough for a
/// busy app to finish a heavy step (opening a large file, a garbage collection), short enough that
/// a person who clicked learns why nothing happens.
pub const UNANSWERED: Duration = Duration::from_secs(5);

/// The least time between two pings to an app that answered: input comes in bursts, and one answer
/// per burst says all there is to know.
pub const PING_SPACING: Duration = Duration::from_secs(1);

/// One client's ping in flight, as plain data: the compositor keeps one per client it pinged and
/// counts its pongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    /// The client's pong count when the ping went out.
    pongs_at_send: u64,
    /// When the ping went out, or when the person last chose "Wait".
    since: Instant,
    /// Whether the app counts as not responding now.
    hung: bool,
}

/// What a [`Watch`] calls for after a look at the client's pongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The ping was answered: the watch ends, and with it any "not responding" state.
    Answered,
    /// Still out, within its time.
    Waiting,
    /// Still out past [`UNANSWERED`]: the app is not responding.
    Hung,
}

impl Watch {
    /// A ping just went out to a client at `pongs` answers.
    #[must_use]
    pub const fn sent(pongs: u64, now: Instant) -> Self {
        Self {
            pongs_at_send: pongs,
            since: now,
            hung: false,
        }
    }

    /// Judge the watch against the client's pong count `pongs` at `now` .
    pub fn judge(&mut self, pongs: u64, now: Instant) -> Verdict {
        if pongs != self.pongs_at_send {
            self.hung = false;
            return Verdict::Answered;
        }
        if now.saturating_duration_since(self.since) >= UNANSWERED {
            self.hung = true;
            Verdict::Hung
        } else {
            Verdict::Waiting
        }
    }

    /// The person chose "Wait": not responding no longer shows, and shows again only after another
    /// [`UNANSWERED`] without an answer.
    pub const fn wait(&mut self, now: Instant) {
        self.since = now;
        self.hung = false;
    }

    /// When this watch next needs a look: the moment it would turn hung. `None` once it is hung;
    /// the answer wakes the loop by itself.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        (!self.hung).then_some(self.since.checked_add(UNANSWERED).unwrap_or(self.since))
    }

    #[must_use]
    pub const fn is_hung(&self) -> bool {
        self.hung
    }
}

/// Pongs counted per `xdg_wm_base` , the only way a client answers a ping.
#[derive(Default)]
pub struct Pongs(Vec<(ShellClient, u64)>);

impl Pongs {
    /// `client` answered a ping.
    pub fn note(&mut self, client: ShellClient) {
        self.0.retain(|(known, _)| known.alive());
        match self.0.iter_mut().find(|(known, _)| *known == client) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => self.0.push((client, 1)),
        }
    }

    /// How many pings `client` answered.
    #[must_use]
    pub fn count(&self, client: &ShellClient) -> u64 {
        self.0
            .iter()
            .find(|(known, _)| known == client)
            .map_or(0, |(_, count)| *count)
    }

    /// Forget `client` , which is gone.
    pub fn forget(&mut self, client: &ShellClient) {
        self.0.retain(|(known, _)| known != client);
    }
}

/// One client's ping in flight, with the connection it belongs to.
struct ClientWatch {
    client: ClientId,
    watch: Option<Watch>,
    last_sent: Instant,
}

/// The compositor's pings in flight, one entry per client it pinged.
#[derive(Default)]
pub struct Watches(Vec<ClientWatch>);

impl Watches {
    /// Input reached `client` (whose connection is `id` ), at `pongs` answers: ping it, unless a
    /// ping is already out or the last went out under [`PING_SPACING`] ago.
    pub fn note_input(&mut self, client: &ShellClient, id: &ClientId, pongs: u64, now: Instant) {
        let index = if let Some(index) = self.0.iter().position(|entry| entry.client == *id) {
            index
        } else {
            self.0.push(ClientWatch {
                client: id.clone(),
                watch: None,
                last_sent: now.checked_sub(PING_SPACING).unwrap_or(now),
            });
            self.0.len().saturating_sub(1)
        };
        let Some(entry) = self.0.get_mut(index) else {
            return;
        };
        if entry.watch.is_some() || now.saturating_duration_since(entry.last_sent) < PING_SPACING {
            return;
        }
        // A ping already pending is answered like the one this would send.
        match client.send_ping(SERIAL_COUNTER.next_serial()) {
            Ok(()) | Err(PingError::PingAlreadyPending(_)) => {
                entry.watch = Some(Watch::sent(pongs, now));
                entry.last_sent = now;
            }
            Err(PingError::DeadSurface) => {}
        }
    }

    /// The person chose "Wait" for the app of connection `id` .
    pub fn wait(&mut self, id: &ClientId, now: Instant) {
        if let Some(watch) = self
            .0
            .iter_mut()
            .find(|entry| entry.client == *id)
            .and_then(|entry| entry.watch.as_mut())
        {
            watch.wait(now);
        }
    }

    /// Stop watching connection `id` : its app is gone.
    pub fn forget(&mut self, id: &ClientId) {
        self.0.retain(|entry| entry.client != *id);
    }

    /// Judge every ping in flight against each client's pong count,
    /// `pongs` (`None` once the client is gone, which ends its watch).
    pub fn judge(&mut self, pongs: impl Fn(&ClientId) -> Option<u64>, now: Instant) {
        self.0.retain_mut(|entry| {
            let Some(count) = pongs(&entry.client) else {
                return false;
            };
            if let Some(watch) = entry.watch.as_mut()
                && watch.judge(count, now) == Verdict::Answered
            {
                entry.watch = None;
            }
            true
        });
    }

    /// The connections whose app is not responding now.
    pub fn hung(&self) -> Vec<ClientId> {
        self.0
            .iter()
            .filter(|entry| entry.watch.is_some_and(|watch| watch.is_hung()))
            .map(|entry| entry.client.clone())
            .collect()
    }

    /// When the next ping in flight would turn hung.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.0
            .iter()
            .filter_map(|entry| entry.watch.as_ref().and_then(Watch::deadline))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start
            .checked_add(Duration::from_millis(ms))
            .expect("in range")
    }

    #[test]
    fn an_unanswered_ping_turns_hung_after_its_time_and_an_answer_clears_it() {
        let start = Instant::now();
        let mut watch = Watch::sent(3, start);
        assert_eq!(watch.judge(3, at(start, 4_999)), Verdict::Waiting);
        assert_eq!(watch.deadline(), Some(at(start, 5_000)));
        assert_eq!(watch.judge(3, at(start, 5_000)), Verdict::Hung);
        assert!(watch.is_hung());
        assert_eq!(watch.deadline(), None, "the answer wakes the loop itself");
        assert_eq!(watch.judge(4, at(start, 9_000)), Verdict::Answered);
        assert!(!watch.is_hung(), "recovery clears the state by itself");
    }

    #[test]
    fn wait_hides_not_responding_for_another_full_period() {
        let start = Instant::now();
        let mut watch = Watch::sent(0, start);
        assert_eq!(watch.judge(0, at(start, 6_000)), Verdict::Hung);
        watch.wait(at(start, 6_000));
        assert!(!watch.is_hung());
        assert_eq!(watch.judge(0, at(start, 10_999)), Verdict::Waiting);
        assert_eq!(watch.judge(0, at(start, 11_000)), Verdict::Hung);
    }
}
