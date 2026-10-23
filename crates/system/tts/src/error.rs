//! Error type for backend construction.
//!
//! Note the asymmetry: *constructing* a backend can fail (no daemon, no binary)
//! and that failure is meaningful — the factory uses it to fall through the
//! chain. *Speaking* never returns an error to the caller; backends log and
//! swallow so the reader stays alive (a dead TTS must not crash the OS's
//! accessibility layer).

use std::io;

/// Errors that can occur while constructing a TTS backend.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    /// The backend's prerequisite (daemon / binary) is not present.
    #[error("tts backend `{backend}` unavailable: {detail}")]
    BackendUnavailable {
        backend: &'static str,
        /// Why it's unavailable.
        detail: String,
    },

    /// Failed to spawn a subprocess (espeak-ng) or connect a socket.
    #[error("tts backend `{backend}` failed to start a process")]
    Spawn {
        backend: &'static str,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The speech-dispatcher SSIP handshake / connection failed.
    #[error("speech-dispatcher (SSIP) connection failed: {detail}")]
    Ssip {
        /// What went wrong.
        detail: String,
    },
}
