//! Key-to-present latency of the windows that receive typing.
//!
//! A key press delivered to a client's focused surface starts a measurement.
//! The client's next buffer commit on that surface answers it; the next frame
//! the compositor submits carries the answer, and the presenter retiring that
//! frame (the page flip, on direct scanout) puts it on the panel. Each answered
//! key is one `KEY-LATENCY` line in the session log, which
//! `tilo-session-report --key-latency` aggregates per app.
//!
//! The client is not asked which commit answers the key: the first commit
//! after it does. A commit the key did not cause (a caret blink) can stand in,
//! which can only make a reading shorter, never invent a stall.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

/// A key no commit answers within this long is reported unanswered and dropped,
/// so a modifier or a key the app ignores cannot claim a much later repaint.
const UNANSWERED_AFTER: Duration = Duration::from_secs(5);
/// Bounds the frames waiting for retirement if a presenter never retires.
const MAX_IN_FLIGHT_FRAMES: usize = 16;
/// Bounds the answers waiting for a submitted frame.
const MAX_UNSUBMITTED_ANSWERS: usize = 64;

/// How often the session log gets one line summing the keys typed since the
/// last: the latency people feel, as a number a regression moves.
const SUMMARY_EVERY: Duration = Duration::from_secs(600);
/// The bar key-to-photon latency is held to.
const KEY_TO_PRESENT_BAR_US: u128 = 20_000;

/// evdev keycodes that change no text by themselves.
const MODIFIER_KEYCODES: [u32; 10] = [29, 42, 54, 56, 58, 97, 100, 125, 126, 127];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Answered {
    app: String,
    key_at: Instant,
    commit_at: Instant,
}

/// One key's journey, in microseconds from its delivery to the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySample {
    pub app: String,
    pub key_to_commit_us: u128,
    pub key_to_submit_us: u128,
    pub key_to_present_us: u128,
}

impl std::fmt::Display for KeySample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "KEY-LATENCY app={} key_to_commit_us={} key_to_submit_us={} key_to_present_us={}",
            self.app, self.key_to_commit_us, self.key_to_submit_us, self.key_to_present_us
        )
    }
}

#[derive(Debug, Default)]
pub struct KeyLatency {
    /// The earliest unanswered key per surface, with the app it went to.
    pending: HashMap<u64, (String, Instant)>,
    /// Answered keys not yet in a submitted frame.
    answered: Vec<Answered>,
    /// Submitted frames carrying answers: serial, submit time, answers.
    in_flight: VecDeque<(u64, Instant, Vec<Answered>)>,
    /// Key-to-present times since the last summary line.
    window: LatencyWindow,
}

/// Key-to-present times gathered for one `KEY-LATENCY-SUMMARY` line.
#[derive(Debug, Default)]
struct LatencyWindow {
    started: Option<Instant>,
    present_us: Vec<u128>,
}

impl LatencyWindow {
    fn note(&mut self, at: Instant, present_us: u128) {
        self.started.get_or_insert(at);
        self.present_us.push(present_us);
    }

    /// The summary of the keys since the window opened, once it has been
    /// open [`SUMMARY_EVERY`]; the next window opens with the next key.
    #[allow(
        clippy::cast_precision_loss,
        reason = "microsecond latencies and key counts, far below f64's exact range, shown to a tenth"
    )]
    fn summary(&mut self, at: Instant) -> Option<String> {
        let started = self.started?;
        let open = at.saturating_duration_since(started);
        if open < SUMMARY_EVERY {
            return None;
        }
        let mut samples = std::mem::take(&mut self.present_us);
        self.started = None;
        samples.sort_unstable();
        let percentile = |p: usize| -> f64 {
            let rank = samples
                .len()
                .saturating_mul(p)
                .div_ceil(100)
                .saturating_sub(1)
                .min(samples.len().saturating_sub(1));
            samples.get(rank).copied().unwrap_or_default() as f64 / 1000.0
        };
        let under = samples
            .iter()
            .filter(|&&us| us < KEY_TO_PRESENT_BAR_US)
            .count();
        let under_pct = if samples.is_empty() {
            0.0
        } else {
            under as f64 * 100.0 / samples.len() as f64
        };
        Some(format!(
            "KEY-LATENCY-SUMMARY window_s={} n={} p50_ms={:.1} p90_ms={:.1} p99_ms={:.1} max_ms={:.1} under_20ms_pct={under_pct:.1}",
            open.as_secs(),
            samples.len(),
            percentile(50),
            percentile(90),
            percentile(99),
            percentile(100),
        ))
    }
}

impl KeyLatency {
    /// A key press reached `surface` of `app`. Modifiers start nothing.
    pub fn key_delivered(&mut self, surface: u64, app: &str, keycode: u32, at: Instant) {
        if MODIFIER_KEYCODES.contains(&keycode) {
            return;
        }
        self.pending
            .entry(surface)
            .or_insert_with(|| (app.to_owned(), at));
    }

    /// `surface` committed a new buffer at `at`. Returns the keys that waited
    /// past [`UNANSWERED_AFTER`] and were dropped, by app.
    pub fn surface_committed(&mut self, surface: u64, at: Instant) -> Vec<String> {
        let mut unanswered = Vec::new();
        self.pending.retain(|&id, (app, key_at)| {
            if id == surface {
                return true;
            }
            let stale = at.saturating_duration_since(*key_at) > UNANSWERED_AFTER;
            if stale {
                unanswered.push(app.clone());
            }
            !stale
        });
        if let Some((app, key_at)) = self.pending.remove(&surface) {
            // A presenter without frame serials never submits these.
            if self.answered.len() == MAX_UNSUBMITTED_ANSWERS {
                self.answered.remove(0);
            }
            self.answered.push(Answered {
                app,
                key_at,
                commit_at: at,
            });
        }
        unanswered
    }

    /// The compositor submitted frame `serial` at `at`; it carries every answer so far.
    pub fn frame_submitted(&mut self, serial: u64, at: Instant) {
        if self.answered.is_empty() {
            return;
        }
        if self.in_flight.len() == MAX_IN_FLIGHT_FRAMES {
            self.in_flight.pop_front();
        }
        self.in_flight
            .push_back((serial, at, std::mem::take(&mut self.answered)));
    }

    /// The presenter retired every frame up to `serial` at `at`.
    pub fn frames_retired(&mut self, serial: u64, at: Instant) -> Vec<KeySample> {
        let mut samples = Vec::new();
        while self
            .in_flight
            .front()
            .is_some_and(|(submitted, _, _)| *submitted <= serial)
        {
            let Some((_, submit_at, answers)) = self.in_flight.pop_front() else {
                break;
            };
            samples.extend(answers.into_iter().map(|answer| {
                KeySample {
                    key_to_commit_us: answer
                        .commit_at
                        .saturating_duration_since(answer.key_at)
                        .as_micros(),
                    key_to_submit_us: submit_at
                        .saturating_duration_since(answer.key_at)
                        .as_micros(),
                    key_to_present_us: at.saturating_duration_since(answer.key_at).as_micros(),
                    app: answer.app,
                }
            }));
        }
        for sample in &samples {
            self.window.note(at, sample.key_to_present_us);
        }
        samples
    }

    /// One line summing the keys of the last ten minutes, when they are due.
    pub fn summary(&mut self, at: Instant) -> Option<String> {
        self.window.summary(at)
    }
}

/// What in the shell took a key, for the `SLOW-KEY` line that reports a key
/// whose handling held the loop: which shortcut ran, or which surface's key
/// routing handled it. Set by the handler, read once per key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyHandler {
    /// A desktop shortcut's action.
    Shortcut(settings_store::hardware::ShortcutAction),
    /// A surface's own key routing (the search palette, the overview).
    Surface(&'static str),
}

thread_local! {
    static KEY_HANDLER: std::cell::Cell<Option<KeyHandler>> = const { std::cell::Cell::new(None) };
}

/// The shell is handling the current key as `handler`.
pub fn note_key_handler(handler: KeyHandler) {
    KEY_HANDLER.with(|slot| slot.set(Some(handler)));
}

/// What handled the key since the last call, clearing it.
pub fn take_key_handler() -> Option<KeyHandler> {
    KEY_HANDLER.with(std::cell::Cell::take)
}

/// The session's tracker. Input, commits and presentation all run on the
/// compositor thread; the lock is uncontended.
pub fn tracker() -> std::sync::MutexGuard<'static, KeyLatency> {
    static TRACKER: OnceLock<Mutex<KeyLatency>> = OnceLock::new();
    TRACKER.get_or_init(Mutex::default).lock_recover()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_key_is_timed_from_delivery_to_the_flip_that_shows_its_answer() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Phase `the_first_commit_after_a_key_answers_it_and_the_retiring_frame_presents_it`.
        {
            let mut latency = KeyLatency::default();
            latency.key_delivered(7, "so.tilo.notes", 30, t0);
            latency.key_delivered(7, "so.tilo.notes", 31, ms(2));
            latency.frame_submitted(1, ms(3));
            assert!(latency.frames_retired(1, ms(4)).is_empty(), "no answer yet");
            assert!(latency.surface_committed(9, ms(5)).is_empty());
            assert!(latency.surface_committed(7, ms(12)).is_empty());
            latency.frame_submitted(2, ms(14));
            assert!(latency.frames_retired(1, ms(15)).is_empty());
            assert_eq!(
                latency.frames_retired(3, ms(30)),
                vec![KeySample {
                    app: "so.tilo.notes".to_owned(),
                    key_to_commit_us: 12_000,
                    key_to_submit_us: 14_000,
                    key_to_present_us: 30_000,
                }],
                "the earliest unanswered key is the one timed"
            );
        }
        // Phase `modifiers_start_nothing_and_ignored_keys_expire`.
        {
            let mut latency = KeyLatency::default();
            latency.key_delivered(7, "so.tilo.notes", 42, t0);
            latency.surface_committed(7, ms(1));
            latency.frame_submitted(1, ms(2));
            assert!(latency.frames_retired(1, ms(3)).is_empty());
            latency.key_delivered(8, "so.tilo.settings", 30, t0);
            assert_eq!(
                latency.surface_committed(7, ms(6_000)),
                vec!["so.tilo.settings".to_owned()]
            );
            latency.surface_committed(8, ms(6_001));
            latency.frame_submitted(2, ms(6_002));
            assert!(latency.frames_retired(2, ms(6_003)).is_empty());
        }
    }

    /// Ten minutes of typing become one line of percentiles and the share
    /// under the 20 ms bar; no line until the window is full, and none for
    /// a window with no keys.
    #[test]
    fn the_log_sums_ten_minutes_of_keys_in_one_line() {
        let t0 = Instant::now();
        let mut window = LatencyWindow::default();
        assert_eq!(window.summary(t0 + SUMMARY_EVERY), None, "no keys, no line");
        for (index, ms) in [10_u128, 15, 30, 60].into_iter().enumerate() {
            window.note(
                t0 + Duration::from_secs(u64::try_from(index).unwrap_or_default()),
                ms.saturating_mul(1000),
            );
        }
        assert_eq!(window.summary(t0 + Duration::from_secs(60)), None);
        let line = window
            .summary(t0 + SUMMARY_EVERY)
            .expect("a full window sums its keys");
        assert_eq!(
            line,
            "KEY-LATENCY-SUMMARY window_s=600 n=4 p50_ms=15.0 p90_ms=60.0 p99_ms=60.0 max_ms=60.0 under_20ms_pct=50.0"
        );
        assert_eq!(
            window.summary(t0 + SUMMARY_EVERY + SUMMARY_EVERY),
            None,
            "the next window starts empty"
        );
    }
}
