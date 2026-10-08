//! PCM decode bridge to the host's output-only stream; no audio socket is opened here.
use super::{Observation, Output, open};
use gstreamer::{self as gst, prelude::*};
use gstreamer_app as gst_app;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Session {
    output: Option<Box<dyn Output>>,
    flushed: u64,
    paused: bool,
    observation: Observation,
    error: Option<String>,
}
pub struct Bridge {
    session: Mutex<Session>,
    pause: AtomicBool,
    flush: AtomicU64,
    closed: AtomicBool,
    reset: AtomicBool,
}
impl Bridge {
    fn controls(&self, session: &mut Session) -> Result<(), String> {
        if self.reset.swap(false, Ordering::AcqRel) {
            if let Some(output) = session.output.as_mut() {
                output.close();
            }
            session.output = None;
            session.observation = Observation::default();
            session.error = None;
        }
        let Some(output) = session.output.as_mut() else {
            return Ok(());
        };
        if self.closed.load(Ordering::Acquire) {
            output.close();
            return Err("Audio output was closed".into());
        }
        let paused = self.pause.load(Ordering::Acquire);
        if session.paused != paused {
            output.pause(paused)?;
            session.paused = paused;
        }
        let flush = self.flush.load(Ordering::Acquire);
        if session.flushed != flush {
            output.flush()?;
            session.flushed = flush;
        }
        Ok(())
    }
    fn push(&self, bytes: &[u8]) -> Result<(), String> {
        if !bytes.len().is_multiple_of(4) {
            return Err("Output PCM is not whole stereo S16 frames".into());
        }
        let mut session = self
            .session
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        self.controls(&mut session)?;
        if session.output.is_none() {
            session.output = Some(open()?);
            session.paused = false;
        }
        self.controls(&mut session)?;
        for packet in bytes.chunks(super::MAX_SAMPLES * 2) {
            let samples: Vec<i16> = packet
                .chunks_exact(2)
                .filter_map(|bytes| <[u8; 2]>::try_from(bytes).ok().map(i16::from_le_bytes))
                .collect();
            let output = session
                .output
                .as_mut()
                .ok_or("Audio output session ended")?;
            session.observation = output.write(&samples)?;
            self.controls(&mut session)?;
        }
        drop(session);
        Ok(())
    }
    /// Local controls only enqueue host commands, so callers never wait on RPC.
    pub fn pause(&self, paused: bool) {
        self.pause.store(paused, Ordering::Release);
        self.pump_controls();
    }
    pub fn flush(&self) {
        self.flush.fetch_add(1, Ordering::AcqRel);
        self.pump_controls();
    }
    /// Retire the previous source after its decoder has stopped. The next PCM opens a fresh generation.
    pub fn reset(&self) {
        self.reset.store(true, Ordering::Release);
        self.pump_controls();
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut session) = self.session.try_lock()
            && let Some(output) = session.output.as_mut()
        {
            output.close();
        }
    }
    fn pump_controls(&self) {
        if let Ok(mut session) = self.session.try_lock()
            && let Err(error) = self.controls(&mut session)
        {
            session.error = Some(error);
        }
    }
    pub fn observation(&self) -> Observation {
        self.session.try_lock().map_or_else(
            |_| Observation::default(),
            |session| {
                session
                    .output
                    .as_ref()
                    .map_or(session.observation, |output| output.cached_observation())
            },
        )
    }
    /// True only after an observed host output has consumed its queued PCM.
    pub fn drained(&self) -> bool {
        let observed = self.observation();
        observed.ready && observed.queued_frames == 0
    }
    pub fn error(&self) -> Option<String> {
        self.session.try_lock().ok().and_then(|session| {
            session.error.clone().or_else(|| {
                session
                    .output
                    .as_ref()
                    .and_then(|output| output.cached_error())
            })
        })
    }
    /// Refresh actual host progress on a decoder worker, never on the UI thread.
    ///
    /// # Errors
    /// Returns a terminal output or device failure.
    pub fn refresh(&self) -> Result<Observation, String> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        let Some(output) = session.output.as_mut() else {
            return Ok(Observation::default());
        };
        let observed = output.observation()?;
        session.observation = observed;
        drop(session);
        Ok(observed)
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.close();
    }
}

/// Build a clocked S16LE/48 kHz/stereo sink with bounded decoder backpressure.
///
/// # Errors
/// Returns a plugin or pipeline construction failure.
pub fn sink_bin(name: &str, gain_name: &str) -> Result<(gst::Bin, Arc<Bridge>), String> {
    let convert = gst::ElementFactory::make("audioconvert")
        .build()
        .map_err(|_| "Audio conversion unavailable")?;
    let resample = gst::ElementFactory::make("audioresample")
        .build()
        .map_err(|_| "Audio resampling unavailable")?;
    let gain = gst::ElementFactory::make("volume")
        .name(gain_name)
        .build()
        .map_err(|_| "Audio gain unavailable")?;
    let sink = gst_app::AppSink::builder()
        .name(name)
        .sync(true)
        .max_buffers(1)
        .drop(false)
        .wait_on_eos(false)
        .caps(
            &gst::Caps::builder("audio/x-raw")
                .field("format", "S16LE")
                .field("layout", "interleaved")
                .field("rate", 48_000_i32)
                .field("channels", 2_i32)
                .build(),
        )
        .build();
    let bridge = Arc::new(Bridge {
        session: Mutex::new(Session {
            output: None,
            flushed: 0,
            paused: false,
            observation: Observation::default(),
            error: None,
        }),
        pause: AtomicBool::new(false),
        flush: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        reset: AtomicBool::new(false),
    });
    let delivered = bridge.clone();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let bytes = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                if let Err(error) = delivered.push(bytes.as_slice()) {
                    if let Ok(mut session) = delivered.session.lock() {
                        session.error = Some(error.clone());
                    }
                    gst::element_error!(
                        sink,
                        gst::ResourceError::Write,
                        ("Governed audio output failed"),
                        ["{error}"]
                    );
                    return Err(gst::FlowError::Error);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let bin = gst::Bin::new();
    bin.add_many([&convert, &resample, &gain, sink.upcast_ref()])
        .map_err(|_| "Audio output assembly failed")?;
    gst::Element::link_many([&convert, &resample, &gain, sink.upcast_ref()])
        .map_err(|_| "Audio output linking failed")?;
    let target = convert
        .static_pad("sink")
        .ok_or("Audio output input pad unavailable")?;
    let pad =
        gst::GhostPad::with_target(&target).map_err(|_| "Audio output ghost pad unavailable")?;
    bin.add_pad(&pad)
        .map_err(|_| "Audio output input pad assembly failed")?;
    Ok((bin, bridge))
}
