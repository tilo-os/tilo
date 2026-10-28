//! A source's sound as interleaved `f32` samples, decoded through
//! `uridecodebin ! audioconvert ! audioresample ! appsink`.
//!
//! The one native audio decode: an editor's mixer collects the whole stream,
//! a waveform reduces it to peaks as it arrives. Either way the caller picks
//! the rate and channel count, so the samples are directly usable whatever
//! the file held.
//!
//! [`Playback`] is the other direction: mixed samples out to the speakers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::source::Source;

/// How a [`decode`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmEnd {
    /// The whole stream was decoded.
    Finished,
    /// The source has no audio stream. A silent video is not an error.
    NoAudio,
    /// The pipeline failed: unreadable bytes, an unknown container, no
    /// decoder for the codec.
    Failed,
    /// The budget ran out with the stream still going.
    TimedOut,
}

/// Whether a pull that came back empty means the source has no audio at all.
///
/// It means that only once the demuxer has finished exposing its pads and
/// none of them was audio. On its own an empty pull proves nothing about the
/// file: `uridecodebin` has to typefind and autoplug before anything reaches
/// the sink, and on a busy machine that takes longer than one pull's timeout.
fn no_audio_concluded(pads_settled: bool, audio_linked: bool) -> bool {
    pads_settled && !audio_linked
}

/// Decode `source`'s first audio stream to interleaved `f32` at `rate` with
/// `channels` channels, handing each decoded buffer to `on_samples`.
///
/// Stops at the end of the stream, at a pipeline error (which never produces
/// another buffer, so waiting on one would only sit out the budget), once
/// the demuxer has said none of its streams is audio, or when `budget` runs
/// out — a wedged pipeline must not hang the caller.
pub fn decode(
    source: &Source,
    rate: u32,
    channels: u32,
    budget: Duration,
    mut on_samples: impl FnMut(&[f32]),
) -> PcmEnd {
    let Ok(uri) = source.to_uri() else {
        return PcmEnd::Failed;
    };
    if gst::init().is_err() {
        return PcmEnd::Failed;
    }
    let Some((pipeline, sink, linked, settled)) = build(&uri, rate, channels) else {
        return PcmEnd::Failed;
    };
    let end = pull(&pipeline, &sink, &linked, &settled, budget, &mut on_samples);
    let _ = pipeline.set_state(gst::State::Null);
    if end != PcmEnd::Finished {
        tracing::debug!(?end, "pcm: decoding {uri} ended early");
    }
    end
}

type Built = (
    gst::Pipeline,
    gst_app::AppSink,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
);

fn build(uri: &str, rate: u32, channels: u32) -> Option<Built> {
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()
        .ok()?;
    let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("channels", i32::try_from(channels).ok()?)
                .field("rate", i32::try_from(rate).ok()?)
                .field("layout", "interleaved")
                .build(),
        )
        .sync(false)
        .build();
    pipeline
        .add_many([&src, &convert, &resample, sink.upcast_ref()])
        .ok()?;
    gst::Element::link_many([&convert, &resample, sink.upcast_ref()]).ok()?;

    // `uridecodebin` exposes pads once it knows the streams; take the first
    // audio one and leave video alone.
    let linked = Arc::new(AtomicBool::new(false));
    let convert_weak = convert.downgrade();
    let linked_in = Arc::clone(&linked);
    src.connect_pad_added(move |_, pad| {
        let is_audio = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if !is_audio {
            return;
        }
        if let Some(c) = convert_weak.upgrade()
            && let Some(target) = c.static_pad("sink")
            && (target.is_linked() || pad.link(&target).is_ok())
        {
            linked_in.store(true, Ordering::SeqCst);
        }
    });
    // `no-more-pads` is the demuxer saying it has finished finding streams.
    // Until it fires, "the sink has produced nothing" is a statement about
    // the clock rather than about the file.
    let settled = Arc::new(AtomicBool::new(false));
    let settled_in = Arc::clone(&settled);
    src.connect_no_more_pads(move |_| settled_in.store(true, Ordering::SeqCst));

    pipeline.set_state(gst::State::Playing).ok()?;
    Some((pipeline, sink, linked, settled))
}

fn pull(
    pipeline: &gst::Pipeline,
    sink: &gst_app::AppSink,
    linked: &AtomicBool,
    settled: &AtomicBool,
    budget: Duration,
    on_samples: &mut impl FnMut(&[f32]),
) -> PcmEnd {
    let bus = pipeline.bus();
    let started = std::time::Instant::now();
    let mut got_any = false;
    let mut words = Vec::new();
    loop {
        if started.elapsed() >= budget {
            return PcmEnd::TimedOut;
        }
        if let Some(bus) = bus.as_ref()
            && let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = msg.view()
        {
            tracing::warn!("pcm: decoding failed: {}", e.error());
            return PcmEnd::Failed;
        }
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(200)) {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Ok(map) = buffer.map_readable() else {
                continue;
            };
            words.clear();
            words.extend(
                map.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&w| f32::from_le_bytes(w)),
            );
            got_any |= !words.is_empty();
            on_samples(&words);
        } else {
            if sink.is_eos() {
                return if got_any {
                    PcmEnd::Finished
                } else {
                    PcmEnd::NoAudio
                };
            }
            // A source with no audio stream never links the sink, so the
            // pull times out forever: only proof of "no audio here" once
            // the demuxer has no more pads to offer.
            if !got_any
                && no_audio_concluded(
                    settled.load(Ordering::SeqCst),
                    linked.load(Ordering::SeqCst),
                )
            {
                return PcmEnd::NoAudio;
            }
        }
    }
}

/// Mixed interleaved stereo `f32` at [`crate::audio_output::RATE`], played
/// on the host's output as it is pushed.
///
/// A hosted app plays through the host's governed output route (it holds no
/// audio device of its own); otherwise `autoaudiosink` picks whatever the
/// machine offers. Pushing blocks while the output's queue is full, which is
/// what paces a mixing thread without a timer of its own.
pub struct Playback {
    out: Out,
}

enum Out {
    Host(Box<dyn crate::audio_output::Output>),
    Local {
        pipeline: gst::Pipeline,
        src: gst_app::AppSrc,
        pushed: u64,
    },
}

impl Playback {
    /// The output, or `None` on a machine with none.
    #[must_use]
    pub fn open() -> Option<Self> {
        if crate::audio_output::is_governed() {
            return match crate::audio_output::open() {
                Ok(out) => Some(Self {
                    out: Out::Host(out),
                }),
                Err(error) => {
                    tracing::info!("pcm: no host audio output: {error}");
                    None
                }
            };
        }
        gst::init().ok()?;
        let pipeline = gst::Pipeline::new();
        let src = gst_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("channels", i32::from(crate::audio_output::CHANNELS))
                    .field("rate", i32::try_from(crate::audio_output::RATE).ok()?)
                    .field("layout", "interleaved")
                    .build(),
            )
            .is_live(true)
            .format(gst::Format::Time)
            .block(true)
            .build();
        let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
        let resample = gst::ElementFactory::make("audioresample").build().ok()?;
        let out = gst::ElementFactory::make("autoaudiosink").build().ok()?;
        pipeline
            .add_many([src.upcast_ref(), &convert, &resample, &out])
            .ok()?;
        gst::Element::link_many([src.upcast_ref(), &convert, &resample, &out]).ok()?;
        pipeline.set_state(gst::State::Playing).ok()?;
        Some(Self {
            out: Out::Local {
                pipeline,
                src,
                pushed: 0,
            },
        })
    }

    /// Play `samples` after what was pushed before. `false` once the output
    /// has gone away.
    pub fn push(&mut self, samples: &[f32]) -> bool {
        match &mut self.out {
            Out::Host(out) => samples
                .chunks(crate::audio_output::MAX_SAMPLES)
                .all(|chunk| out.write(&to_s16(chunk)).is_ok()),
            Out::Local { src, pushed, .. } => {
                let channels = u64::from(crate::audio_output::CHANNELS);
                let rate = u64::from(crate::audio_output::RATE);
                let frames = (samples.len() as u64).checked_div(channels).unwrap_or(0);
                let ns = |f: u64| {
                    f.saturating_mul(1_000_000_000)
                        .checked_div(rate)
                        .unwrap_or(0)
                };
                let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                let mut buffer = gst::Buffer::from_mut_slice(bytes);
                if let Some(b) = buffer.get_mut() {
                    b.set_pts(gst::ClockTime::from_nseconds(ns(*pushed)));
                    b.set_duration(gst::ClockTime::from_nseconds(ns(frames)));
                }
                *pushed = pushed.saturating_add(frames);
                src.push_buffer(buffer).is_ok()
            }
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        match &mut self.out {
            Out::Host(out) => out.close(),
            Out::Local { pipeline, .. } => {
                let _ = pipeline.set_state(gst::State::Null);
            }
        }
    }
}

/// `f32` samples as signed 16-bit, clamped. 32767 rather than 32768: scaling
/// by the negative limit lets +1.0 overflow to -32768, a full-scale click.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to -1..1 first, so the scaled value fits i16"
)]
fn to_s16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_before_the_demuxer_has_settled_is_not_proof_of_no_audio() {
        // Nothing pulled, nothing linked, pads still arriving: an empty pull
        // is a statement about the clock, not the file. A loaded machine
        // produces exactly this.
        assert!(!no_audio_concluded(false, false));
        // Every pad is in and none was audio: now the silence is the file's.
        assert!(no_audio_concluded(true, false));
        // An audio pad is linked, so quiet means it has not got going yet.
        assert!(!no_audio_concluded(true, true));
        assert!(!no_audio_concluded(false, true));
    }

    #[test]
    fn an_unreadable_source_fails_without_waiting_out_the_budget() {
        let dir = tempfile::tempdir().expect("temp dir");
        let junk = dir.path().join("junk.mp4");
        std::fs::write(&junk, b"not a container, not even close").expect("write");
        let mut samples = 0usize;
        let end = decode(
            &Source::LocalFile(junk),
            8_000,
            1,
            Duration::from_secs(600),
            |s| samples = samples.saturating_add(s.len()),
        );
        assert_eq!(samples, 0, "there is no audio in junk");
        assert!(
            matches!(end, PcmEnd::Failed | PcmEnd::NoAudio),
            "the pipeline's own failure ends the wait, got {end:?}"
        );
    }
}
