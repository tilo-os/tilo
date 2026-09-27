//! [`default_speaker`] — the never-panics backend selector.

use crate::null::NullSpeaker;
use crate::speaker::Speaker;

/// Construct the best available [`Speaker`], trying each backend in priority
/// order and falling back gracefully:
///
/// 1. **speech-dispatcher** over SSIP — the primary path (it brokers
///    espeak-ng / Piper system-side and gives us server-side queueing +
///    per-priority interrupt).
/// 2. **espeak-ng** subprocess — instant, low-latency fallback when no daemon is
///    running.
/// 3. **[`NullSpeaker`]** — a no-op recorder so the engine *always* returns
///    something. The reader stays alive even with no speech stack installed; the
///    user just hears nothing (and tests/headless get a recorder).
///
/// This function **never panics** regardless of what's installed.
///
/// Returns a `(Box<dyn Speaker>, &'static str)` so the caller can log which
/// backend won. (The name is also available via [`Speaker::backend_name`].)
pub fn default_speaker() -> Box<dyn Speaker> {
    select_speaker().0
}

/// Like [`default_speaker`] but also returns the chosen backend's name, for
/// diagnostics/tests.
pub fn select_speaker() -> (Box<dyn Speaker>, &'static str) {
    // 1. speech-dispatcher uses a Unix-domain socket. Other platforms retain
    // the portable subprocess backend below.
    #[cfg(unix)]
    match crate::speechd::SpeechDispatcherSpeaker::new() {
        Ok(s) => {
            tracing::info!("tilo-tts: using speech-dispatcher backend");
            return (Box::new(s), "speech-dispatcher");
        }
        Err(e) => tracing::debug!("tilo-tts: speech-dispatcher unavailable: {e}"),
    }

    // 2. espeak-ng subprocess.
    #[cfg(not(target_arch = "wasm32"))]
    match crate::espeak::EspeakSpeaker::new() {
        Ok(s) => {
            tracing::info!("tilo-tts: using espeak-ng subprocess backend");
            return (Box::new(s), "espeak-ng");
        }
        Err(e) => tracing::debug!("tilo-tts: espeak-ng unavailable: {e}"),
    }

    // 3. null.
    tracing::warn!("tilo-tts: no TTS backend available; falling back to NullSpeaker (silent)");
    (Box::new(NullSpeaker::new()), "null")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_factory_always_returns_a_named_speaker() {
        // Phase `factory_always_returns_a_speaker`.
        {
            // Must not panic regardless of whether a daemon / espeak-ng is present.
            let mut s = default_speaker();
            // The returned speaker is usable: speaking must not panic either.
            s.speak("startup", true);
            s.stop();
            // backend_name is one of the known three.
            assert!(matches!(
                s.backend_name(),
                "speech-dispatcher" | "espeak-ng" | "null"
            ));
        }
        // Phase `select_reports_a_known_backend_name`.
        {
            let (_s, name) = select_speaker();
            assert!(matches!(name, "speech-dispatcher" | "espeak-ng" | "null"));
        }
    }
}
