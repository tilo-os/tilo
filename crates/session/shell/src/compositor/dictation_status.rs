//! What dictation reports to the chrome: session identities, failures and
//! status. The overlay that shows them is built on every host; the recognizer
//! and the text insertion behind them (`compositor::dictation`) are Linux-only.

use agent_provider_config::speech::SpeechError;

/// Identifies one recognizer request. Late callbacks cannot affect its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(pub(super) u64);

/// Failures contain no audio, transcript, or field contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationError {
    Busy,
    NoEditableField,
    SensitiveField,
    /// A terminal exposes process input, not a qualified editable inner field.
    UnsupportedField,
    InputMethodUnavailable,
    StaleSession,
    TargetChanged,
    TimedOut,
    DraftTooLarge,
    ControlCharacter,
    SessionIdsExhausted,
    RecognizerUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    VocabularyChanged,
    TooLong,
    Rejected,
    UnknownDelivery,
}

/// Status exposes no recording or draft to a remote collaborator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationStatus {
    #[default]
    Idle,
    Preparing,
    Loading,
    Listening,
    StoppingRecording,
    Finishing,
    Stopping,
    Unavailable(RuntimeError),
    Recovery {
        reason: RecoveryReason,
        copied: bool,
        copy_failed: bool,
    },
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
