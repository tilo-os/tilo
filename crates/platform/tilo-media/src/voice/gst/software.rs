//! A call's audio where no device stack answers: a source that makes a tone
//! and a sink that keeps what a call plays, through the same encoder, frames
//! and playout a device's call uses.
//!
//! Test builds of this crate and its dependents carry it
//! (`--features test-support`): a host with no `PipeWire` or `PulseAudio` still
//! hears a call's tone end to end, and a test reads the samples the call
//! played instead of a speaker's room.

use std::sync::Arc;

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use super::{AudioError, VoiceAudio, element};

pub struct SoftwareOutput {
    audio: VoiceAudio,
    sink: gst_app::AppSink,
}

impl SoftwareOutput {
    pub fn start() -> Result<Self, AudioError> {
        gst::init().map_err(|_| AudioError::Unavailable)?;
        let pipeline = gst::Pipeline::new();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(256)
            .build();
        sink.set_property("async", false);
        sink.set_property("drop", true);
        pipeline
            .add_many([source.upcast_ref(), &convert, &resample, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        gst::Element::link_many([source.upcast_ref(), &convert, &resample, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        let audio = VoiceAudio::from_output(pipeline, source)?;
        Ok(Self { audio, sink })
    }

    pub const fn audio(&mut self) -> &mut VoiceAudio {
        &mut self.audio
    }

    #[must_use]
    pub fn played(&self) -> Vec<f32> {
        let mut played = Vec::new();
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Ok(map) = buffer.map_readable() else {
                continue;
            };
            for bytes in map.as_slice().chunks_exact(2) {
                let Ok([low, high]) = <[u8; 2]>::try_from(bytes) else {
                    continue;
                };
                played.push(f32::from(i16::from_le_bytes([low, high])) / 32_768.0);
            }
        }
        played
    }
}

impl VoiceAudio {
    /// Send the tone `hz` instead of a microphone: a source that keeps making
    /// a voice's frames until capture stops.
    ///
    /// # Errors
    ///
    /// As [`VoiceAudio::start_capture`], and [`AudioError::Unavailable`] when
    /// the stack has no test source.
    pub fn start_software_capture(
        &mut self,
        hz: f64,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), AudioError> {
        gst::init().map_err(|_| AudioError::Unavailable)?;
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("freq", hz)
            .property("volume", 0.3_f64)
            .property("is-live", true)
            .property_from_str("wave", "sine")
            .build()
            .map_err(|_| AudioError::Unavailable)?;
        self.start_capture_element_bound(&source, still_current, None, None)
    }
}
