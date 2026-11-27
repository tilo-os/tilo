//! Error type for backend construction.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("tts backend `{backend}` unavailable: {detail}")]
    BackendUnavailable {
        backend: &'static str,
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
    Ssip { detail: String },
}
