//! Phonon's documented streaming transcript protocol. No engine errors or
//! transcript strings enter diagnostics; callers map typed errors into UI copy.

use serde::Deserialize;

use super::MAX_DRAFT_BYTES;

/// Errors contain neither transcript content nor the upstream error message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeechError {
    MessageTooLarge,
    InvalidMessage,
    TranscriptTooLarge,
    ControlCharacter,
    SegmentOutOfOrder,
    EngineRejected,
    StreamClosed,
    UnsupportedLanguage,
}

/// An in-progress hypothesis or the final complete session result.
#[derive(PartialEq, Eq)]
pub enum SpeechUpdate {
    Draft(String),
    Done(String),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Partial { text: String },
    Final { text: String, segment: u32 },
    Done { text: String },
    // Deliberately discard this untrusted string; do not log it or show it as
    // app-authored UI copy. It can include model paths or recorded words.
    Error { message: serde::de::IgnoredAny },
}

/// Per-request decoder. Replacing partials prevents repeated words; finalized
/// phrases are retained only until the session ends. Use one instance per ID.
#[derive(Default)]
pub struct PhononTranscript {
    finalized: String,
    segments: u32,
    closed: bool,
}

impl PhononTranscript {
    /// Phonon-2 is English-only. Reject a selected language rather than passing
    /// its unimplemented `language` parameter and silently recognizing English.
    ///
    /// # Errors
    /// Returns `UnsupportedLanguage` for any non-English language selection.
    pub fn for_language(language: &str) -> Result<Self, SpeechError> {
        if language.eq_ignore_ascii_case("en")
            || language
                .get(..3)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("en-"))
        {
            Ok(Self::default())
        } else {
            Err(SpeechError::UnsupportedLanguage)
        }
    }

    /// Decode one bounded text message from the ordered WebSocket stream.
    /// `partial` replaces the in-flight phrase, `final` appends a phrase, and
    /// `done` carries the complete corrected transcript. A final segment alone
    /// must never trigger field insertion: only `done` finishes the request.
    ///
    /// # Errors
    /// Fails closed on oversized/invalid frames, control characters, replayed
    /// segments or engine refusal. After failure no further result is accepted.
    pub fn observe(&mut self, frame: &str) -> Result<SpeechUpdate, SpeechError> {
        if self.closed {
            return Err(SpeechError::StreamClosed);
        }
        let result = self.decode(frame);
        if result.is_err() || matches!(result, Ok(SpeechUpdate::Done(_))) {
            self.closed = true;
            self.finalized.clear();
        }
        result
    }

    fn decode(&mut self, frame: &str) -> Result<SpeechUpdate, SpeechError> {
        // Six bytes per JSON-escaped scalar plus bounded envelope overhead.
        if frame.len() > MAX_DRAFT_BYTES.saturating_mul(6).saturating_add(1024) {
            return Err(SpeechError::MessageTooLarge);
        }
        let event: Event = serde_json::from_str(frame).map_err(|_| SpeechError::InvalidMessage)?;
        match event {
            Event::Partial { text } => self.join(&text).map(SpeechUpdate::Draft),
            Event::Final { text, segment } => {
                if self.segments.checked_add(1) != Some(segment) {
                    return Err(SpeechError::SegmentOutOfOrder);
                }
                self.finalized = self.join(&text)?;
                self.segments = segment;
                Ok(SpeechUpdate::Draft(self.finalized.clone()))
            }
            Event::Done { text } => {
                validate_text(&text)?;
                Ok(SpeechUpdate::Done(text))
            }
            Event::Error { message: _message } => Err(SpeechError::EngineRejected),
        }
    }

    fn join(&self, phrase: &str) -> Result<String, SpeechError> {
        validate_text(phrase)?;
        let separator = !self.finalized.is_empty() && !phrase.is_empty();
        let length = self
            .finalized
            .len()
            .saturating_add(phrase.len())
            .saturating_add(usize::from(separator));
        if length > MAX_DRAFT_BYTES {
            return Err(SpeechError::TranscriptTooLarge);
        }
        let mut text = String::with_capacity(length);
        text.push_str(&self.finalized);
        if separator {
            text.push(' ');
        }
        text.push_str(phrase);
        Ok(text)
    }
}

fn validate_text(text: &str) -> Result<(), SpeechError> {
    if text.len() > MAX_DRAFT_BYTES {
        return Err(SpeechError::TranscriptTooLarge);
    }
    if text.chars().any(char::is_control) {
        return Err(SpeechError::ControlCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partials_replace_and_segments_accumulate_without_premature_insertion() {
        let mut stream = PhononTranscript::for_language("en-GB").unwrap();
        assert!(
            matches!(stream.observe(r#"{"type":"partial","text":"rough"}"#), Ok(SpeechUpdate::Draft(text)) if text == "rough")
        );
        assert!(
            matches!(stream.observe(r#"{"type":"partial","text":"Corrected."}"#), Ok(SpeechUpdate::Draft(text)) if text == "Corrected.")
        );
        assert!(
            matches!(stream.observe(r#"{"type":"final","text":"Corrected.","segment":1}"#), Ok(SpeechUpdate::Draft(text)) if text == "Corrected.")
        );
        assert!(
            matches!(stream.observe(r#"{"type":"partial","text":"Next 👋"}"#), Ok(SpeechUpdate::Draft(text)) if text == "Corrected. Next 👋")
        );
        assert!(
            matches!(stream.observe(r#"{"type":"done","text":"Corrected. Next 👋"}"#), Ok(SpeechUpdate::Done(text)) if text == "Corrected. Next 👋")
        );
        assert!(matches!(
            stream.observe(r#"{"type":"done","text":"late"}"#),
            Err(SpeechError::StreamClosed)
        ));
    }

    #[test]
    fn failures_close_the_stream_without_exposing_upstream_copy() {
        for (frame, error) in [
            (
                r#"{"type":"error","message":"private model path and recorded words"}"#,
                SpeechError::EngineRejected,
            ),
            (
                r#"{"type":"final","text":"replay","segment":2}"#,
                SpeechError::SegmentOutOfOrder,
            ),
            (
                r#"{"type":"done","text":"run\n"}"#,
                SpeechError::ControlCharacter,
            ),
            (r#"{"type":"unknown"}"#, SpeechError::InvalidMessage),
        ] {
            let mut stream = PhononTranscript::default();
            assert!(matches!(stream.observe(frame), Err(actual) if actual == error));
            assert!(matches!(
                stream.observe(r#"{"type":"done","text":"late"}"#),
                Err(SpeechError::StreamClosed)
            ));
        }
        assert!(PhononTranscript::for_language("nl").is_err());
        assert!(PhononTranscript::for_language("auto").is_err());
        let mut stream = PhononTranscript::default();
        let oversized = serde_json::json!({"type":"done", "text":"x".repeat(MAX_DRAFT_BYTES.saturating_add(1))}).to_string();
        assert!(matches!(
            stream.observe(&oversized),
            Err(SpeechError::TranscriptTooLarge)
        ));
    }
}
