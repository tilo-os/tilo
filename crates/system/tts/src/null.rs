//! A no-op, recording [`Speaker`] for tests and headless operation.

use crate::speaker::{Speaker, Utterance};

/// A [`Speaker`] that synthesizes nothing but records every interaction.
///
/// Two roles:
/// * **Test double** — assert exactly what the reader *would* have said, and in
///   what order, without any audio or daemon: assert the utterance text
///   produced, no audio needed.
/// * **Headless / no-TTS fallback** — the terminal member of
///   [`crate::default_speaker`]'s chain, so the engine never panics when no
///   speech stack is installed.
#[derive(Debug, Default, Clone)]
pub struct NullSpeaker {
    spoken: Vec<String>,
    utterances: Vec<Utterance>,
    stops: usize,
    rate: f32,
    voice: Option<String>,
    speaking: bool,
}

impl NullSpeaker {
    /// A fresh recorder with rate `1.0` and no voice set.
    pub fn new() -> Self {
        NullSpeaker {
            spoken: Vec::new(),
            utterances: Vec::new(),
            stops: 0,
            rate: 1.0,
            voice: None,
            speaking: false,
        }
    }

    /// The texts spoken, in order. (Convenience over [`Self::utterances`] when a
    /// test only cares about the strings.)
    pub fn spoken(&self) -> &[String] {
        &self.spoken
    }

    /// The full utterances spoken, in order (text + priority + interrupt flag).
    pub fn utterances(&self) -> &[Utterance] {
        &self.utterances
    }

    /// The most recently spoken text, if any.
    pub fn last(&self) -> Option<&str> {
        self.spoken.last().map(String::as_str)
    }

    /// How many times [`Speaker::stop`] was called.
    pub fn stop_count(&self) -> usize {
        self.stops
    }

    /// The current rate (last value passed to [`Speaker::set_rate`]).
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// The current voice id, if [`Speaker::set_voice`] was called.
    pub fn voice(&self) -> Option<&str> {
        self.voice.as_deref()
    }

    /// Forget all recorded interactions (rate/voice persist).
    pub fn reset(&mut self) {
        self.spoken.clear();
        self.utterances.clear();
        self.stops = 0;
        self.speaking = false;
    }
}

impl Speaker for NullSpeaker {
    fn speak_utterance(&mut self, utterance: Utterance) {
        // Interrupting or not, the new utterance is what is now speaking. An
        // interrupt cuts off whatever was "in flight", but the mock keeps the
        // *record* of earlier speech: tests want the history.
        self.speaking = true;
        self.spoken.push(utterance.text.clone());
        self.utterances.push(utterance);
    }

    fn stop(&mut self) {
        self.stops = self.stops.saturating_add(1);
        self.speaking = false;
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
    }

    fn set_voice(&mut self, voice: &str) {
        self.voice = Some(voice.to_string());
    }

    fn is_speaking(&self) -> bool {
        self.speaking
    }

    fn backend_name(&self) -> &'static str {
        "null"
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::speaker::Priority;

    #[test]
    fn the_null_speaker_records_utterances_stops_and_settings() {
        // Phase `records_spoken_text`.
        {
            let mut s = NullSpeaker::new();
            s.speak("hello", true);
            s.speak("world", false);
            assert_eq!(s.spoken(), &["hello".to_string(), "world".to_string()]);
            assert_eq!(s.last(), Some("world"));
        }
        // Phase `records_full_utterance`.
        {
            let mut s = NullSpeaker::new();
            s.speak_utterance(Utterance::new("err").with_priority(Priority::Important));
            assert_eq!(s.utterances().len(), 1);
            assert_eq!(s.utterances()[0].priority, Priority::Important);
            assert!(s.utterances()[0].interrupt);
        }
        // Phase `stop_counts_and_clears_speaking`.
        {
            let mut s = NullSpeaker::new();
            s.speak("hi", true);
            assert!(s.is_speaking());
            s.stop();
            assert_eq!(s.stop_count(), 1);
            assert!(!s.is_speaking());
        }
        // Phase `rate_and_voice_setters`.
        {
            let mut s = NullSpeaker::new();
            assert_eq!(s.rate(), 1.0);
            assert_eq!(s.voice(), None);
            s.set_rate(1.5);
            s.set_voice("en-us");
            assert_eq!(s.rate(), 1.5);
            assert_eq!(s.voice(), Some("en-us"));
        }
        // Phase `reset_keeps_rate_and_voice`.
        {
            let mut s = NullSpeaker::new();
            s.set_rate(2.0);
            s.set_voice("de");
            s.speak("x", true);
            s.stop();
            s.reset();
            assert!(s.spoken().is_empty());
            assert_eq!(s.stop_count(), 0);
            assert_eq!(s.rate(), 2.0);
            assert_eq!(s.voice(), Some("de"));
        }
        // Phase `backend_name`.
        {
            assert_eq!(NullSpeaker::new().backend_name(), "null");
        }
    }
}
