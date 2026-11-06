//! The microphone a dictation records with: the shared `pw-record` capture
//! ([`media::audio::record`]) of the `PipeWire` microphone the machine has as
//! its default source, 16 kHz mono S16LE.
//!
//! Nothing here opens the microphone on its own: [`Microphone::open`] is
//! called only after the person allowed this dictation, and it re-checks that
//! permission after the source probe, so a cancellation during the probe
//! cannot leave a recording running. The capture ends rather than dropping
//! samples the recognizer has not read yet: a gap a recognizer could bridge is
//! a *discontinuity*, a fatal error, never a silent gap from which it could
//! manufacture missing words.

use std::sync::atomic::{AtomicBool, Ordering};

use media::audio::record::{self, Format, Overflow};

/// The source a dictation records from, named by `PipeWire` itself so that a
/// source removed before the stream starts cannot silently fall back to
/// another microphone.
const STREAM_PROPERTIES: &str = "{ node.name = tilo-dictation-microphone node.description = \"Tilo dictation microphone\" node.dont-reconnect = true node.dont-fallback = true node.dont-move = true }";

/// An open dictation microphone.
pub struct Microphone {
    capture: record::Capture,
    stream: record::Stream,
}

impl Microphone {
    /// Open the default microphone, unless `cancel` is set: before the source
    /// probe and again after it. The spawning thread must stay alive until
    /// capture stops — parent death kills the recorder.
    ///
    /// # Errors
    /// When dictation was cancelled, the source could not be resolved, or the
    /// recorder could not be started.
    pub fn open(cancel: &AtomicBool) -> Result<Self, String> {
        if cancel.load(Ordering::Acquire) {
            return Err("dictation canceled".into());
        }
        let target = default_source()?;
        if cancel.load(Ordering::Acquire) {
            return Err("dictation canceled".into());
        }
        let spec = record::Spec {
            target: &target,
            properties: Some(STREAM_PROPERTIES),
            format: Format::MONO_16K,
            overflow: Overflow::Fail,
            label: "dictation microphone",
        };
        let (capture, stream) = record::Capture::start(&spec)?;
        Ok(Self { capture, stream })
    }

    /// Every complete sample produced since the last call. Interrupted audio
    /// is an error: a caller stops dictation rather than guessing.
    ///
    /// # Errors
    /// When the audio queue overflowed, the source ended, or the recognizer
    /// fell behind the recorder.
    pub fn drain(&mut self) -> Result<Vec<u8>, String> {
        if !self.capture.alive() {
            return Err("PipeWire dictation capture ended unexpectedly".into());
        }
        self.stream.take()
    }
}

/// The default source's `object.serial`, resolved through `wpctl`.
fn default_source() -> Result<String, String> {
    media::audio::default_node::object_serial(media::audio::default_node::DefaultNode::Source)
        .map_err(|error| format!("default microphone: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dictation_takes_mono_at_sixteen_kilohertz_without_falling_back() {
        assert_eq!(
            Format::MONO_16K,
            Format {
                rate: 16_000,
                channels: 1
            }
        );
        assert_eq!(Format::MONO_16K.frame_bytes(), 2);
        assert!(STREAM_PROPERTIES.contains("node.dont-fallback = true"));
        assert!(STREAM_PROPERTIES.contains("node.dont-reconnect = true"));
        assert!(STREAM_PROPERTIES.contains("node.name = tilo-dictation-microphone"));
    }

    #[test]
    fn a_cancelled_dictation_never_opens_the_microphone() {
        let cancel = AtomicBool::new(true);
        assert_eq!(
            Microphone::open(&cancel).err(),
            Some("dictation canceled".to_owned())
        );
    }
}
