//! Session identities, refusals and status: what the session and the sheet
//! act on. None of it carries recorded words: a transcript only ever lives in
//! the private draft of [`crate::Session`] and in what is committed into the
//! field.

use agent_provider_config::speech::SpeechError;

/// Identifies one dictation session. Late callbacks and a retired prompt
/// cannot affect its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Why a dictation did not start or continue. None of these carry audio,
/// transcript bytes or field contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationError {
    /// Another dictation, its preparation or its recovery still owns the seat.
    Busy,
    /// No field has the focus and asks for typing.
    NoEditableField,
    /// The field takes a secret: a password, a PIN, digits, hidden text.
    SensitiveField,
    /// A terminal exposes process input, not a field to dictate into.
    UnsupportedField,
    /// The request names a dictation that is over (or never was).
    StaleSession,
    /// The field the person typed into is no longer the one they dictate into.
    TargetChanged,
    /// The dictation outlived its window without a final result.
    TimedOut,
    /// A hypothesis or final beyond the draft bound.
    DraftTooLarge,
    /// Recognition returned a control character (or a line feed outside a
    /// multiline field).
    ControlCharacter,
    /// Every identity has been used.
    SessionIdsExhausted,
    /// The recognizer or its audio stopped, and why: a missing model or an
    /// unsupported language reads differently to the person than a busy
    /// microphone.
    RecognizerUnavailable(RuntimeError),
}

impl DictationError {
    /// A short word for the status reading and the sheet.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::NoEditableField => "no-editable-field",
            Self::SensitiveField => "sensitive-field",
            Self::UnsupportedField => "unsupported-field",
            Self::StaleSession => "stale-session",
            Self::TargetChanged => "target-changed",
            Self::TimedOut => "timed-out",
            Self::DraftTooLarge => "draft-too-large",
            Self::ControlCharacter => "control-character",
            Self::SessionIdsExhausted => "session-ids-exhausted",
            Self::RecognizerUnavailable(_) => "recognizer-unavailable",
        }
    }

    /// Why the recognizer stopped, when [`Self::RecognizerUnavailable`] says
    /// it did: what the sheet names to the person.
    #[must_use]
    pub const fn reason(self) -> Option<RuntimeError> {
        match self {
            Self::RecognizerUnavailable(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Why a transcript was kept instead of inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    /// The person's personal dictionary changed while they dictated, so the
    /// words on screen were recognised with hints that no longer apply.
    VocabularyChanged,
    /// The transcript is longer than a field takes in one edit.
    TooLong,
}

impl RecoveryReason {
    /// A short word for the status reading and the sheet.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::VocabularyChanged => "vocabulary-changed",
            Self::TooLong => "too-long",
        }
    }
}

/// Where a dictation is. Exposes no recording and no draft to anyone else:
/// the live hypothesis is local and private.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationStatus {
    #[default]
    Idle,
    /// Waiting for the person to allow the microphone for this dictation.
    Consent,
    /// Opening the field for typing and warming the recognizer.
    Preparing,
    /// The recognizer is starting and nothing is heard yet.
    Loading,
    /// The microphone is open and words are arriving.
    Listening,
    /// The microphone is stopping.
    StoppingRecording,
    /// The text is being finished.
    Finishing,
    /// The microphone is closed and the worker is going away.
    Stopping,
    /// Nothing ran; the word says why.
    Unavailable(DictationError),
    /// Nothing was inserted; the transcript is kept for the person.
    Recovery {
        reason: RecoveryReason,
        copied: bool,
        copy_failed: bool,
    },
}

impl DictationStatus {
    /// A short word for the status reading and the sheet.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Consent => "consent",
            Self::Preparing => "preparing",
            Self::Loading => "loading",
            Self::Listening => "listening",
            Self::StoppingRecording => "stopping-recording",
            Self::Finishing => "finishing",
            Self::Stopping => "stopping",
            Self::Unavailable(_) => "unavailable",
            Self::Recovery { .. } => "recovery",
        }
    }

    /// Whether a dictation still owns the seat: from consent until its worker
    /// is gone.
    #[must_use]
    pub const fn running(self) -> bool {
        !matches!(self, Self::Idle | Self::Unavailable(_) | Self::Recovery { .. })
    }
}

/// Why the recognizer worker stopped, without any recorded words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Worker(SpeechError),
    MicrophoneUnavailable,
    VocabularyUnavailable,
    VocabularyChanged,
    AudioDiscontinuity,
    TimedOut,
    Protocol,
    Busy,
    Canceled,
}

impl RuntimeError {
    /// A short word for why it stopped, without the engine's own message:
    /// what the sheet names to the person.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Worker(
                SpeechError::ModelUnavailable
                | SpeechError::ModelReceipt,
            ) => "model",
            Self::Worker(SpeechError::UnsupportedLanguage) => "language",
            Self::Worker(_) => "recognizer",
            Self::MicrophoneUnavailable => "microphone",
            Self::VocabularyUnavailable | Self::VocabularyChanged => "vocabulary",
            Self::AudioDiscontinuity => "audio",
            Self::TimedOut => "timed-out",
            Self::Protocol => "protocol",
            Self::Busy => "busy",
            Self::Canceled => "canceled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dictation_owns_the_seat_from_consent_until_its_worker_is_gone() {
        for status in [
            DictationStatus::Consent,
            DictationStatus::Preparing,
            DictationStatus::Loading,
            DictationStatus::Listening,
            DictationStatus::StoppingRecording,
            DictationStatus::Finishing,
            DictationStatus::Stopping,
        ] {
            assert!(status.running(), "{status:?} is a running dictation");
        }
        for status in [
            DictationStatus::Idle,
            DictationStatus::Unavailable(DictationError::RecognizerUnavailable(
                RuntimeError::Canceled,
            )),
            DictationStatus::Recovery {
                reason: RecoveryReason::TooLong,
                copied: false,
                copy_failed: false,
            },
        ] {
            assert!(!status.running(), "{status:?} is over");
        }
    }
}
