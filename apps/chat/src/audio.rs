//! Call audio: microphone capture, Opus encode/decode, and speaker playback.
//!
//! Everything here is **`GStreamer`** — `autoaudiosrc`/`autoaudiosink` for the
//! system devices and `opusenc`/`opusdec` for the codec — so no ALSA / Pulse /
//! `opus` crate is a direct dependency (the Opus library is reached only as a
//! `GStreamer` element). The whole module is compiled only under `live-media`.
//!
//! Two layers:
//! * [`OpusEncoder`] / [`OpusDecoder`] — device-free codec pipelines driven by
//!   `appsrc`/`appsink`. They Opus-encode raw PCM and decode it back with no
//!   audio hardware, so the transport + codec contract is exercised fully
//!   headless (see the loopback test in [`crate::media`]).
//! * [`MicCapture`] / [`SpeakerPlayback`] — the live device pipelines a real
//!   call uses: the mic → Opus packets to publish, and received Opus → the
//!   speakers. Both return `None` from `new` when no device is present, so a
//!   headless run degrades to silence rather than failing.
//!
//! Calls are mono 48 kHz — the standard Opus voice configuration.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// Sample rate for all call audio (Opus's fullband rate).
pub const RATE: i32 = 48_000;
/// Channel count for call audio (mono voice).
pub const CHANNELS: i32 = 1;
/// Opus frame duration; the codec reframes input to this granularity.
const FRAME_MS: u64 = 20;

/// Samples in one 20 ms mono frame (`48000 * 0.02`).
fn frame_samples() -> usize {
    (RATE as usize / 1000) * FRAME_MS as usize * CHANNELS as usize
}

fn ensure_init() -> bool {
    gst::init().is_ok()
}

fn raw_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("rate", RATE)
        .field("channels", CHANNELS)
        .field("layout", "interleaved")
        .build()
}

/// The minimal, streamheader-free Opus caps a decoder needs to accept raw
/// packets pulled off the wire (mapping family 0 = standard mono/stereo).
fn opus_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-opus")
        .field("rate", RATE)
        .field("channels", CHANNELS)
        .field("channel-mapping-family", 0i32)
        .build()
}

fn pcm_to_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn bytes_to_pcm(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// A deterministic mono PCM sine tone of `ms` milliseconds — the stand-in for a
/// live microphone in the headless codec/transport test. `rate`/`channels` are
/// accepted for symmetry with the call config (which is always 48 kHz mono).
pub fn test_tone(rate: i32, channels: i32, ms: u64) -> Vec<i16> {
    let n = (rate as u64 * ms / 1000) as usize * channels as usize;
    let mut out = Vec::with_capacity(n);
    let freq = 440.0_f64;
    for i in 0..n {
        let t = i as f64 / f64::from(rate);
        let v = (t * freq * std::f64::consts::TAU).sin();
        out.push((v * 12_000.0) as i16);
    }
    out
}

/// Pull every currently-available sample from an appsink into byte buffers,
/// stopping when the sink signals end-of-stream or no sample arrives within the
/// timeout. Never blocks longer than `per_pull` per empty poll.
fn drain_appsink(sink: &gst_app::AppSink, per_pull: gst::ClockTime) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match sink.try_pull_sample(per_pull) {
            Some(sample) => {
                if let Some(buffer) = sample.buffer()
                    && let Ok(map) = buffer.map_readable()
                {
                    out.push(map.as_slice().to_vec());
                }
            }
            None => {
                // No sample within the timeout: either the stream has ended and
                // fully drained, or nothing more is coming — either way, done.
                break;
            }
        }
    }
    out
}

fn appsrc_of(pipeline: &gst::Pipeline, name: &str) -> Option<gst_app::AppSrc> {
    pipeline.by_name(name)?.downcast::<gst_app::AppSrc>().ok()
}

fn appsink_of(pipeline: &gst::Pipeline, name: &str) -> Option<gst_app::AppSink> {
    pipeline.by_name(name)?.downcast::<gst_app::AppSink>().ok()
}

/// Opus encoder: raw mono 48 kHz PCM → a sequence of Opus packets. Device-free
/// (`appsrc`→`opusenc`→`appsink`), so it runs anywhere `GStreamer` is installed.
pub struct OpusEncoder {
    _init: (),
}

impl OpusEncoder {
    /// Build an encoder, verifying the `opusenc` element is present. `None` when
    /// `GStreamer` can't initialise or the Opus plugin is missing.
    pub fn new() -> Option<Self> {
        if !ensure_init() {
            return None;
        }
        gst::ElementFactory::make("opusenc").build().ok()?;
        Some(Self { _init: () })
    }

    /// Encode PCM into Opus packets. The input is split into exact 20 ms frames
    /// (the tail padded with silence) and fed with proper timestamps so
    /// `opusenc` frames it cleanly; each returned `Vec<u8>` is one Opus packet.
    /// Returns empty on any pipeline failure — never panics.
    pub fn encode(&mut self, pcm: &[i16]) -> Vec<Vec<u8>> {
        let Some(packets) = self.try_encode(pcm) else {
            return Vec::new();
        };
        packets
    }

    fn try_encode(&mut self, pcm: &[i16]) -> Option<Vec<Vec<u8>>> {
        let pipeline = gst::parse::launch(
            "appsrc name=src is-live=false format=time ! audioconvert ! audioresample \
             ! opusenc ! appsink name=sink sync=false",
        )
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
        let src = appsrc_of(&pipeline, "src")?;
        let sink = appsink_of(&pipeline, "sink")?;
        src.set_caps(Some(&raw_caps()));
        src.set_property("do-timestamp", false);
        src.set_format(gst::Format::Time);

        pipeline.set_state(gst::State::Playing).ok()?;

        let per_frame = frame_samples();
        let mut idx = 0u64;
        let mut i = 0usize;
        while i < pcm.len() {
            let end = (i + per_frame).min(pcm.len());
            let mut frame = pcm[i..end].to_vec();
            if frame.len() < per_frame {
                frame.resize(per_frame, 0); // pad tail with silence
            }
            let mut buffer = gst::Buffer::from_slice(pcm_to_bytes(&frame));
            {
                let b = buffer.get_mut().unwrap();
                b.set_pts(gst::ClockTime::from_mseconds(idx * FRAME_MS));
                b.set_duration(gst::ClockTime::from_mseconds(FRAME_MS));
            }
            src.push_buffer(buffer).ok()?;
            idx += 1;
            i = end;
        }
        let _ = src.end_of_stream();

        let packets = drain_appsink(&sink, gst::ClockTime::from_mseconds(300));
        let _ = pipeline.set_state(gst::State::Null);
        Some(packets)
    }
}

/// Opus decoder: Opus packets → raw mono 48 kHz PCM samples. Device-free
/// (`appsrc`→`opusdec`→`appsink`), the receive-side mirror of [`OpusEncoder`].
pub struct OpusDecoder {
    _init: (),
}

impl OpusDecoder {
    pub fn new() -> Option<Self> {
        if !ensure_init() {
            return None;
        }
        gst::ElementFactory::make("opusdec").build().ok()?;
        Some(Self { _init: () })
    }

    /// Decode one Opus packet into interleaved i16 PCM. Returns empty on any
    /// failure — never panics.
    pub fn decode(&mut self, packet: &[u8]) -> Vec<i16> {
        self.try_decode(packet).unwrap_or_default()
    }

    fn try_decode(&mut self, packet: &[u8]) -> Option<Vec<i16>> {
        if packet.is_empty() {
            return Some(Vec::new());
        }
        let pipeline = gst::parse::launch(
            "appsrc name=src format=time ! opusdec ! audioconvert ! audioresample \
             ! appsink name=sink sync=false",
        )
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
        let src = appsrc_of(&pipeline, "src")?;
        let sink = appsink_of(&pipeline, "sink")?;
        src.set_caps(Some(&opus_caps()));
        src.set_format(gst::Format::Time);
        sink.set_caps(Some(&raw_caps()));

        pipeline.set_state(gst::State::Playing).ok()?;

        let mut buffer = gst::Buffer::from_slice(packet.to_vec());
        {
            let b = buffer.get_mut().unwrap();
            b.set_pts(gst::ClockTime::ZERO);
            b.set_duration(gst::ClockTime::from_mseconds(FRAME_MS));
        }
        src.push_buffer(buffer).ok()?;
        let _ = src.end_of_stream();

        let mut pcm = Vec::new();
        for raw in drain_appsink(&sink, gst::ClockTime::from_mseconds(300)) {
            pcm.extend(bytes_to_pcm(&raw));
        }
        let _ = pipeline.set_state(gst::State::Null);
        Some(pcm)
    }
}

/// Live microphone → Opus packets. A continuous pipeline
/// (`autoaudiosrc`→`opusenc`→`appsink`) that a running call polls each frame for
/// freshly-encoded packets to publish. `None` when there is no capture device.
pub struct MicCapture {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
}

impl MicCapture {
    pub fn new() -> Option<Self> {
        if !ensure_init() {
            return None;
        }
        let pipeline = gst::parse::launch(
            "autoaudiosrc ! audioconvert ! audioresample \
             ! audio/x-raw,rate=48000,channels=1 ! opusenc ! appsink name=sink sync=false max-buffers=8 drop=true",
        )
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
        let sink = appsink_of(&pipeline, "sink")?;
        // A missing/blocked capture device fails the state change → no capture.
        if pipeline.set_state(gst::State::Playing).is_err() {
            return None;
        }
        Some(Self { pipeline, sink })
    }

    /// Non-blocking: return any Opus packets captured since the last poll.
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(buffer) = sample.buffer()
                && let Ok(map) = buffer.map_readable()
            {
                out.push(map.as_slice().to_vec());
            }
        }
        out
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Received Opus packets → speakers. A continuous pipeline
/// (`appsrc`→`opusdec`→`autoaudiosink`) fed one packet at a time. `None` when
/// there is no output device.
pub struct SpeakerPlayback {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    pts_ms: u64,
}

impl SpeakerPlayback {
    pub fn new() -> Option<Self> {
        if !ensure_init() {
            return None;
        }
        let pipeline = gst::parse::launch(
            "appsrc name=src is-live=true format=time ! opusdec ! audioconvert ! audioresample \
             ! autoaudiosink",
        )
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
        let src = appsrc_of(&pipeline, "src")?;
        src.set_caps(Some(&opus_caps()));
        src.set_format(gst::Format::Time);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return None;
        }
        Some(Self {
            pipeline,
            src,
            pts_ms: 0,
        })
    }

    /// Push one received Opus packet to the speakers.
    pub fn play(&mut self, packet: &[u8]) {
        if packet.is_empty() {
            return;
        }
        let mut buffer = gst::Buffer::from_slice(packet.to_vec());
        {
            let b = buffer.get_mut().unwrap();
            b.set_pts(gst::ClockTime::from_mseconds(self.pts_ms));
            b.set_duration(gst::ClockTime::from_mseconds(FRAME_MS));
        }
        self.pts_ms += FRAME_MS;
        let _ = self.src.push_buffer(buffer);
    }
}

impl Drop for SpeakerPlayback {
    fn drop(&mut self) {
        let _ = self.src.end_of_stream();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_bytes_round_trip() {
        let pcm = vec![0i16, 1, -1, 32767, -32768, 12345];
        assert_eq!(bytes_to_pcm(&pcm_to_bytes(&pcm)), pcm);
    }

    #[test]
    fn test_tone_has_expected_length_and_signal() {
        let t = test_tone(RATE, CHANNELS, 40);
        assert_eq!(t.len(), (RATE as usize / 1000) * 40, "40 ms of 48 kHz mono");
        assert!(
            t.iter().any(|&s| s.abs() > 1000),
            "tone actually oscillates"
        );
    }

    /// Real codec round-trip with no audio device: PCM → Opus → PCM. Proves the
    /// `GStreamer` Opus encode/decode contract the call transport carries.
    #[test]
    fn opus_encode_decode_round_trips_without_a_device() {
        let (Some(mut enc), Some(mut dec)) = (OpusEncoder::new(), OpusDecoder::new()) else {
            eprintln!("GStreamer Opus elements unavailable; skipping codec round-trip");
            return;
        };
        let pcm = test_tone(RATE, CHANNELS, 60); // 3 × 20 ms frames
        let packets = enc.encode(&pcm);
        assert!(!packets.is_empty(), "opusenc produced packets");
        assert!(
            packets.iter().all(|p| !p.is_empty()),
            "each packet carries bytes"
        );
        let mut decoded = 0usize;
        for p in &packets {
            decoded += dec.decode(p).len();
        }
        assert!(
            decoded > 0,
            "opusdec recovered PCM samples from the encoded packets"
        );
    }
}
