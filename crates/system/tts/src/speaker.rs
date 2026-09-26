//! The [`Speaker`] trait and the supporting [`Priority`] / [`Utterance`]
//! model shared by every backend.

/// Priority of an utterance, modelled after speech-dispatcher's SSIP priority levels (see
/// `man spd-say`, the `SET … PRIORITY` SSIP command).
///
/// The screen reader maps interaction events onto these:
///
/// * [`Priority::Important`] — errors, permission prompts, modal entry. Spoken immediately
///   and *never* discarded; later `Important` messages don't cancel an in-progress one.
/// * [`Priority::Message`] — focus-change narration, "where am I", explicit requests. The
///   workhorse level; a new `Message` cancels any pending `Message`/`Text` so chatty focus
///   changes coalesce.
/// * [`Priority::Text`] — bulk content ("say all", read-from-cursor). Lowest "speak it" tier;
///   interrupted freely by focus changes.
/// * [`Priority::Notification`] — short, transient (toast-style `Live::Polite`). Spoken only
///   when nothing higher is queued; superseded silently.
/// * [`Priority::Progress`] — progress chatter; only the most recent survives.
///
/// Ordering is *significance* ascending: `Important > Message > Text > Notification >
/// Progress`, so the queue can compare with `>=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    /// Progress chatter; only the most recent survives.
    Progress,
    /// Ambient notification — yields to anything the user asked for.
    Notification,
    /// Document or control text the user is reading.
    Text,
    /// The default: something the app is saying to the user.
    #[default]
    Message,
    /// Interrupts whatever is speaking.
    Important,
}

#[cfg(unix)]
impl Priority {
    /// Map to the `ssip` crate's [`ssip_client::Priority`]. Kept here (rather than
    /// a `From` impl on the foreign type) so the rest of the crate never has to
    /// name the foreign enum.
    #[cfg(unix)]
    pub(crate) fn to_ssip(self) -> ssip_client::Priority {
        use ssip_client::Priority as P;
        match self {
            Priority::Progress => P::Progress,
            Priority::Notification => P::Notification,
            Priority::Text => P::Text,
            Priority::Message => P::Message,
            Priority::Important => P::Important,
        }
    }
}

/// A single thing to say, plus how it should be scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    /// The text to synthesize. Plain text; SSML is intentionally out of scope.
    pub text: String,
    /// Scheduling priority.
    pub priority: Priority,
    /// If `true`, speaking this utterance stops whatever is currently being spoken (and clears
    /// lower-or-equal-priority pending work) — the responsive-focus-narration knob.
    pub interrupt: bool,
}

impl Utterance {
    /// A normal-priority, interrupting utterance — the common case for focus narration.
    pub fn new(text: impl Into<String>) -> Self {
        Utterance {
            text: text.into(),
            priority: Priority::Message,
            interrupt: true,
        }
    }

    /// Builder: set the priority.
    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Builder: set whether this utterance interrupts current speech.
    #[must_use]
    pub fn with_interrupt(mut self, interrupt: bool) -> Self {
        self.interrupt = interrupt;
        self
    }
}

/// A text-to-speech backend. Expected to be cheap to call and to *never panic* on a missing
/// or dead daemon — failures are logged and swallowed so the reader keeps running.
/// Construction is where "is TTS available?" is decided (see [`crate::default_speaker`]);
/// once you hold a `Speaker`, calling it is infallible from the caller's perspective.
pub trait Speaker: Send {
    /// Speak `text`. If `interrupt` is `true`, stop current speech first (equivalent to
    /// `speak_utterance` with `interrupt = true` at the default [`Priority::Message`]).
    fn speak(&mut self, text: &str, interrupt: bool) {
        self.speak_utterance(Utterance::new(text).with_interrupt(interrupt));
    }

    /// Speak a fully-specified [`Utterance`] (priority + interrupt semantics).
    fn speak_utterance(&mut self, utterance: Utterance);

    /// Stop all speech immediately and discard anything pending — the "silence key" (Orca
    /// convention: tap Ctrl to shut it up).
    fn stop(&mut self);

    /// Set the speaking rate: a normalized multiplier where `1.0` is engine default, `2.0`
    /// roughly twice as fast, `0.5` half; out-of-range values are clamped by the backend. (SSIP's
    /// native scale is an `i8` in `[-100, 100]`; backends translate.)
    fn set_rate(&mut self, rate: f32);

    /// Select a voice by backend-specific identifier (e.g. an espeak-ng voice like `"en-us"`, or
    /// a speech-dispatcher synthesis-voice name).
    fn set_voice(&mut self, voice: &str);

    /// Whether the backend currently has speech in progress or queued.
    fn is_speaking(&self) -> bool;

    /// Human-readable backend name, for diagnostics/logging.
    fn backend_name(&self) -> &'static str;
}

/// Translate a normalized rate multiplier (`1.0` = default) to SSIP's `i8` `[-100, 100]`
/// scale (`1.0 -> 0`, `2.0 -> +100`, `0.5 -> -100`), using a log-ish piecewise-linear map so
/// `0.5`/`2.0` are symmetric extremes, matching how speech-dispatcher perceives rate.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to [-100.0, 100.0] just above, well within i8's range"
)]
#[cfg(any(unix, test))]
pub(crate) fn rate_to_ssip(rate: f32) -> i8 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    // 1.0 -> 0. Faster than default maps onto (0, 100], slower onto [-100, 0).
    let scaled = if rate >= 1.0 {
        // 1.0..=2.0 -> 0..=100, saturating beyond.
        ((rate - 1.0) * 100.0).round()
    } else {
        // 0.5..1.0 -> -100..0, saturating below.
        ((rate - 1.0) * 200.0).round()
    };
    scaled.clamp(-100.0, 100.0) as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_utterances_and_rates_have_documented_defaults() {
        // Phase `priority_ordering_is_significance_ascending`.
        {
            assert!(Priority::Important > Priority::Message);
            assert!(Priority::Message > Priority::Text);
            assert!(Priority::Text > Priority::Notification);
            assert!(Priority::Notification > Priority::Progress);
        }
        // Phase `default_priority_is_message`.
        {
            assert_eq!(Priority::default(), Priority::Message);
        }
        // Phase `utterance_builder`.
        {
            let u = Utterance::new("hi")
                .with_priority(Priority::Important)
                .with_interrupt(false);
            assert_eq!(u.text, "hi");
            assert_eq!(u.priority, Priority::Important);
            assert!(!u.interrupt);
        }
        // Phase `utterance_new_defaults_to_interrupting_message`.
        {
            let u = Utterance::new("focus changed");
            assert_eq!(u.priority, Priority::Message);
            assert!(u.interrupt);
        }
        // Phase `rate_mapping`.
        {
            assert_eq!(rate_to_ssip(1.0), 0);
            assert_eq!(rate_to_ssip(2.0), 100);
            assert_eq!(rate_to_ssip(3.0), 100); // saturates
            assert_eq!(rate_to_ssip(0.5), -100);
            assert_eq!(rate_to_ssip(0.1), -100); // saturates
            assert_eq!(rate_to_ssip(0.0), 0); // guarded
            assert_eq!(rate_to_ssip(-1.0), 0); // guarded
            assert_eq!(rate_to_ssip(f32::NAN), 0); // guarded
            assert_eq!(rate_to_ssip(1.5), 50);
            assert_eq!(rate_to_ssip(0.75), -50);
        }
    }

    #[test]
    #[cfg(unix)]
    fn priority_to_ssip_total() {
        // Just prove every variant maps without panicking.
        for p in [
            Priority::Progress,
            Priority::Notification,
            Priority::Text,
            Priority::Message,
            Priority::Important,
        ] {
            let _ = p.to_ssip();
        }
    }
}
