//! # tilo-tts — the Tilo text-to-speech engine
//!
//! | Backend | Type | Role |
//! |---|---|---|
//! | `SpeechDispatcherSpeaker` (Unix) | SSIP socket to `speech-dispatcher` | **primary** — brokers espeak-ng / Piper system-side, server-side queue + interrupt |
//! | [`EspeakSpeaker`] | `espeak-ng` subprocess | low-latency fallback when no daemon is running |
//! | [`NullSpeaker`] | no-op recorder | tests + headless; never panics |
//!
//! [`default_speaker`] tries them in that order and *always* returns something
//! (it never panics if no TTS is installed).

#![forbid(unsafe_code)]
mod error;
#[cfg(not(target_arch = "wasm32"))]
mod espeak;
mod factory;
mod null;
mod queue;
mod speaker;
#[cfg(unix)]
mod speechd;

pub use error::TtsError;
#[cfg(not(target_arch = "wasm32"))]
pub use espeak::EspeakSpeaker;
pub use factory::{default_speaker, select_speaker};
pub use null::NullSpeaker;
pub use queue::SpeechQueue;
pub use speaker::{Priority, Speaker, Utterance};
#[cfg(unix)]
pub use speechd::SpeechDispatcherSpeaker;

#[cfg(test)]
mod integration_tests {
    //! Cross-module behaviour: the queue + a recording backend modelling what the reader
    //! actually does, with no daemon and no audio.
    use super::*;

    #[test]
    fn speech_through_the_trait_object_coalesces_stops_and_keeps_settings() {
        {
            // Simulate a fast Tab through a toolbar: five focus changes arrive before anything is
            // spoken. Only the last should survive (interrupt semantics).
            let mut q = SpeechQueue::new();
            for label in ["New", "Open", "Save", "Print", "Close"] {
                q.push(Utterance::new(format!("{label} button")));
            }
            assert_eq!(q.len(), 1);
            let mut spk = NullSpeaker::new();
            q.drive(&mut spk);
            assert_eq!(spk.spoken(), &["Close button".to_string()]);
        }
        {
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("Permission denied").with_priority(Priority::Important));
            q.push(Utterance::new("Allow button")); // Message interrupt
            let mut spk = NullSpeaker::new();
            q.drive(&mut spk);
            assert_eq!(
                spk.spoken(),
                &["Permission denied".to_string(), "Allow button".to_string()]
            );
        }
        {
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("a long say-all").with_priority(Priority::Text));
            let mut spk = NullSpeaker::new();
            spk.speak("currently talking", true);
            assert!(spk.is_speaking());

            spk.stop();
            q.clear();

            assert!(!spk.is_speaking());
            assert!(q.is_empty());
            assert_eq!(spk.stop_count(), 1);
        }
        {
            let mut spk: Box<dyn Speaker> = Box::new(NullSpeaker::new());
            spk.set_rate(1.5);
            spk.set_voice("en-us");
            // Down-cast not needed; behaviour is observable via the concrete mock in its own tests.
            // Here we just prove the trait-object path doesn't panic.
            spk.speak("hi", true);
            assert!(spk.is_speaking());
        }
    }
}
