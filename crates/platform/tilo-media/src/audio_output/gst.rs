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
    observed: Mutex<Observation>,
    pause: AtomicBool,
    flush: AtomicU64,
    closed: AtomicBool,
    reset: AtomicBool,
}
impl Bridge {
    fn cache_observation(&self, observed: Observation) -> Observation {
        if let Ok(mut cached) = self.observed.lock() {
            *cached = observed;
        }
        observed
    }

    fn controls(&self, session: &mut Session, apply_pause: bool) -> Result<(), String> {
        if self.reset.swap(false, Ordering::AcqRel) {
            if let Some(output) = session.output.as_mut() {
                output.close();
            }
            session.output = None;
            session.observation = self.cache_observation(Observation::default());
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
        if session.paused != paused && (apply_pause || !paused) {
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
        self.controls(&mut session, false)?;
        if session.output.is_none() {
            session.output = Some(open()?);
            session.paused = false;
        }
        self.controls(&mut session, false)?;
        for packet in bytes.chunks(super::MAX_SAMPLES * 2) {
            let samples: Vec<i16> = packet
                .chunks_exact(2)
                .filter_map(|bytes| <[u8; 2]>::try_from(bytes).ok().map(i16::from_le_bytes))
                .collect();
            let output = session
                .output
                .as_mut()
                .ok_or("Audio output session ended")?;
            session.observation = self.cache_observation(output.write(&samples)?);
            self.controls(&mut session, false)?;
        }
        // Finish the current decoded buffer while the host still consumes its
        // bounded queue. Pausing between its packets can strand the remainder
        // behind a full queue that has just stopped draining.
        self.controls(&mut session, true)?;
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
            && let Err(error) = self.controls(&mut session, true)
        {
            session.error = Some(error);
        }
    }
    pub fn observation(&self) -> Observation {
        if self.reset.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) {
            return Observation::default();
        }
        self.session.try_lock().map_or_else(
            // A decoder may be waiting for host backpressure while holding the
            // session. Preserve its last host acknowledgment, without waiting
            // on that RPC or inventing progress from submitted samples.
            |_| {
                self.observed
                    .lock()
                    .map_or_else(|_| Observation::default(), |observed| *observed)
            },
            |session| {
                self.cache_observation(
                    session
                        .output
                        .as_ref()
                        .map_or(session.observation, |output| output.cached_observation()),
                )
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
    pub fn refresh(&self) -> Result<Observation, String> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| "Audio output state unavailable")?;
        let Some(output) = session.output.as_mut() else {
            return Ok(Observation::default());
        };
        let observed = output.observation()?;
        session.observation = self.cache_observation(observed);
        drop(session);
        Ok(observed)
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.close();
    }
}

/// Build a pipeline-clocked S16LE/48 kHz/stereo sink for synchronized media.
pub fn sink_bin(name: &str, gain_name: &str) -> Result<(gst::Bin, Arc<Bridge>), String> {
    build_sink_bin(name, gain_name, true)
}

/// Build an audio-only sink paced by the host device and its bounded PCM queue.
pub fn host_clocked_sink_bin(
    name: &str,
    gain_name: &str,
) -> Result<(gst::Bin, Arc<Bridge>), String> {
    build_sink_bin(name, gain_name, false)
}

fn build_sink_bin(
    name: &str,
    gain_name: &str,
    sync: bool,
) -> Result<(gst::Bin, Arc<Bridge>), String> {
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
        // The host device consumes PCM at its own clock and bounds its queue.
        // Waiting for each buffer's pipeline timestamp before starting its
        // output RPC leaves no lookahead for admission or transport jitter.
        // Let the host's bounded write backpressure pace decoding instead.
        .sync(sync)
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
        observed: Mutex::new(Observation::default()),
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

#[cfg(test)]
mod tests {
    use super::*;

    struct ObservedOutput(std::sync::mpsc::Sender<Vec<i16>>);
    impl Output for ObservedOutput {
        fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
            self.0
                .send(samples.to_vec())
                .map_err(|error| error.to_string())?;
            Ok(Observation {
                rendered_frames: 0,
                queued_frames: 960,
                ready: true,
            })
        }
        fn observation(&mut self) -> Result<Observation, String> {
            Ok(self.cached_observation())
        }
        fn cached_observation(&self) -> Observation {
            Observation {
                rendered_frames: 0,
                queued_frames: 960,
                ready: true,
            }
        }
        fn cached_error(&self) -> Option<String> {
            None
        }
        fn pause(&mut self, _paused: bool) -> Result<(), String> {
            Ok(())
        }
        fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn volume(&mut self, _percent: u8) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self) {}
    }

    #[test]
    fn decoded_pcm_reaches_host_queue_before_its_pipeline_timestamp() {
        gst::init().expect("actual output bridge plugins available");
        let (sink, bridge) = host_clocked_sink_bin("pacing-test", "pacing-gain")
            .expect("actual output bridge constructed");
        let (sent, received) = std::sync::mpsc::channel();
        bridge.session.lock().expect("session").output = Some(Box::new(ObservedOutput(sent)));
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "S16LE")
                    .field("layout", "interleaved")
                    .field("rate", 48_000_i32)
                    .field("channels", 2_i32)
                    .build(),
            )
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([
                source.upcast_ref::<gst::Element>(),
                sink.upcast_ref::<gst::Element>(),
            ])
            .expect("pipeline");
        source.link(&sink).expect("production audio bridge linked");
        pipeline
            .set_state(gst::State::Playing)
            .expect("start pipeline");
        let mut buffer = gst::Buffer::from_slice(vec![0x41_u8; 3840]);
        let data = buffer.get_mut().expect("owned PCM buffer");
        data.set_pts(gst::ClockTime::from_seconds(10));
        data.set_duration(gst::ClockTime::from_mseconds(20));
        source
            .push_buffer(buffer)
            .expect("submit future decoded PCM");
        let delivered = received.recv_timeout(tilo_test_env::hang_bound());
        pipeline.set_state(gst::State::Null).expect("stop pipeline");
        drop(pipeline);
        drop(bridge);
        assert_eq!(
            delivered.expect("the host queue, rather than the pipeline clock, paces output"),
            vec![0x4141_i16; 1920],
            "the production callback preserves every stereo PCM sample"
        );
    }

    struct PausingOutput {
        bridge: std::sync::Weak<Bridge>,
        paused: bool,
        delivered: Arc<AtomicU64>,
    }
    impl Output for PausingOutput {
        fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
            if self.paused {
                return Err("a full paused host queue cannot accept more PCM".into());
            }
            self.delivered
                .fetch_add(samples.len() as u64, Ordering::Relaxed);
            self.bridge.upgrade().expect("live bridge").pause(true);
            Ok(self.cached_observation())
        }
        fn observation(&mut self) -> Result<Observation, String> {
            Ok(self.cached_observation())
        }
        fn cached_observation(&self) -> Observation {
            Observation {
                rendered_frames: 0,
                queued_frames: 7680,
                ready: true,
            }
        }
        fn cached_error(&self) -> Option<String> {
            None
        }
        fn pause(&mut self, paused: bool) -> Result<(), String> {
            self.paused = paused;
            Ok(())
        }
        fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn volume(&mut self, _percent: u8) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self) {}
    }

    #[test]
    fn pause_waits_for_the_current_decoded_buffer_to_reach_the_host() {
        gst::init().expect("actual output bridge plugins available");
        let (_bin, bridge) =
            sink_bin("pause-test", "pause-gain").expect("actual output bridge constructed");
        let delivered = Arc::new(AtomicU64::new(0));
        bridge.session.lock().expect("session").output = Some(Box::new(PausingOutput {
            bridge: Arc::downgrade(&bridge),
            paused: false,
            delivered: delivered.clone(),
        }));
        bridge
            .push(&vec![0x41; super::super::MAX_SAMPLES * 4])
            .expect("pause cannot strand a partially submitted decoded buffer");
        assert_eq!(
            delivered.load(Ordering::Relaxed),
            (super::super::MAX_SAMPLES * 2) as u64
        );
        assert!(
            bridge.session.lock().expect("session").paused,
            "pause takes effect at the complete decoded-buffer boundary"
        );
    }

    #[test]
    fn decoder_backpressure_preserves_observed_host_progress() {
        gst::init().expect("actual output bridge plugins available");
        let (_bin, bridge) =
            sink_bin("progress-test", "progress-gain").expect("actual output bridge constructed");
        let observed = Observation {
            rendered_frames: 960,
            queued_frames: 1920,
            ready: true,
        };
        bridge
            .session
            .lock()
            .expect("test output session available")
            .observation = observed;
        assert_eq!(bridge.observation(), observed);
        let _decoder_rpc = bridge
            .session
            .lock()
            .expect("test output session available");
        assert_eq!(
            bridge.observation(),
            observed,
            "an in-flight decoder write must not erase confirmed host progress"
        );
    }

    #[test]
    fn source_reset_and_close_retire_cached_host_progress() {
        gst::init().expect("actual output bridge plugins available");
        for close in [false, true] {
            let (_bin, bridge) = sink_bin("retirement-test", "retirement-gain")
                .expect("actual output bridge constructed");
            let observed = Observation {
                rendered_frames: 960,
                queued_frames: 1920,
                ready: true,
            };
            bridge
                .session
                .lock()
                .expect("test output session available")
                .observation = observed;
            assert_eq!(bridge.observation(), observed);
            let _decoder_rpc = bridge
                .session
                .lock()
                .expect("test output session available");
            if close {
                bridge.close();
            } else {
                bridge.reset();
            }
            assert_eq!(
                bridge.observation(),
                Observation::default(),
                "a retired source must not reuse its prior host progress"
            );
        }
    }
}
