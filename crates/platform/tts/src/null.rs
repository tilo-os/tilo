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

    pub fn utterances(&self) -> &[Utterance] {
        &self.utterances
    }

    pub fn last(&self) -> Option<&str> {
        self.spoken.last().map(String::as_str)
    }

    pub fn stop_count(&self) -> usize {
        self.stops
    }

    pub fn rate(&self) -> f32 {
        self.rate
    }

    pub fn voice(&self) -> Option<&str> {
        self.voice.as_deref()
    }

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
