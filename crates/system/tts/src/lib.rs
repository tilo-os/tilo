//! # tilo-tts — the Tilo text-to-speech engine
//!
//! A small, dependency-light, **offline** speech engine for the Tilo built-in screen reader.
//! It abstracts over several speech backends behind one [`Speaker`] trait and adds an
//! interrupt-aware [`SpeechQueue`] so chatty focus narration coalesces and preempts sanely.
//!
//! ## Backends
//!
//! | Backend | Type | Role |
//! |---|---|---|
//! | `SpeechDispatcherSpeaker` (Unix) | SSIP socket to `speech-dispatcher` | **primary** — brokers espeak-ng / Piper system-side, server-side queue + interrupt |
//! | [`EspeakSpeaker`] | `espeak-ng` subprocess | low-latency fallback when no daemon is running |
//! | [`NullSpeaker`] | no-op recorder | tests + headless; never panics |
//!
//! [`default_speaker`] tries them in that order and *always* returns something
//! (it never panics if no TTS is installed).
//!
//! ## Quick start
//!
//! ```no_run
//! use tts::{default_speaker, Speaker, Utterance, Priority};
//!
//! let mut speaker = default_speaker();          // never panics
//! speaker.set_rate(1.2);
//! speaker.speak("Save, button", true);          // interrupt = stop-and-say
//! // Or with explicit priority:
//! speaker.speak_utterance(
//!     Utterance::new("Permission denied").with_priority(Priority::Important),
//! );
//! ```
//!
//! With the queue (what the reader uses):
//!
//! ```
//! use tts::{SpeechQueue, NullSpeaker, Utterance, Speaker};
//!
//! let mut queue = SpeechQueue::new();
//! let mut speaker = NullSpeaker::new();
//!
//! queue.push(Utterance::new("Toolbar"));        // focus change (interrupting)
//! queue.push(Utterance::new("Save button"));    // newer focus change supersedes
//! queue.drive(&mut speaker);
//! assert_eq!(speaker.spoken(), &["Save button".to_string()]);
//! ```
//!
//! ## License posture
//!
//! This crate is `MIT OR Apache-2.0` and links **only permissive** code. The actual speech
//! engines are GPL, but each is reached as a **separate process across a protocol/process
//! boundary**, which is *not* linkage and imposes **no GPL constraint** on this crate or
//! anything that links it:
//!
//! * **speech-dispatcher** (GPL): reached over its **SSIP socket** via the pure-Rust
//!   [`ssip-client`](https://crates.io/crates/ssip-client) crate (MIT OR Apache-2.0).
//! * **espeak-ng** (GPL-3.0): reached as a **spawned subprocess** (text piped to its stdin);
//!   also typically reached *through* speech-dispatcher as an output module.
//! * **Piper** (OHF-Voice fork, GPL-3.0): reached as a **speech-dispatcher output module** —
//!   another separate process behind the SSIP socket.
//!
//! Ship those engines in the OS image and document their licenses; the Tilo code itself stays
//! permissive. (`piper-plus`, MIT, exists as a GPL-free in-process escape hatch if a
//! daemon-less embedded path is ever needed — out of scope here.)
//!
//! All backends are offline: no network, no telemetry.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
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
        // Phase `focus_storm_coalesces_to_latest`.
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
        // Phase `error_survives_focus_change_then_both_spoken`.
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
        // Phase `stop_clears_queue_and_silences`.
        {
            // The "silence key": stop() on the speaker + clear() on the queue.
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("a long say-all").with_priority(Priority::Text));
            let mut spk = NullSpeaker::new();
            spk.speak("currently talking", true);
            assert!(spk.is_speaking());

            // Silence:
            spk.stop();
            q.clear();

            assert!(!spk.is_speaking());
            assert!(q.is_empty());
            assert_eq!(spk.stop_count(), 1);
        }
        // Phase `rate_and_voice_round_trip_through_trait_object`.
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
