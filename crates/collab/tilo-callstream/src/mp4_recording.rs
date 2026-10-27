//! In-process MP4/MOV sink for screen recordings.
//!
//! The capture path hands over compressed access units from the media
//! stack's real-time encoder; this module muxes them with `GStreamer`
//! (`appsrc ! <parser> ! mp4mux|qtmux ! filesink`) without a second encode.
//! Recordings with audio are MOV (`qtmux`), which carries the lossless S16LE
//! tracks as they were captured; video-only recordings are MP4.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use gstreamer_app as gst_app;
use media::StreamCodec;
use media::gst;
use media::gst::prelude::*;

/// How long finishing may take to write the trailer.
const FINISH_TIMEOUT: Duration = Duration::from_secs(30);

/// The codecs a recording tries, most compatible first: H.264 (every player
/// takes it; hardware or Cisco's openh264), then AV1 in hardware, then VP9
/// in software, which always exists.
fn recording_codecs() -> Vec<StreamCodec> {
    let caps = media::realtime::local_capabilities();
    let mut out = Vec::new();
    if caps.can_encode(StreamCodec::H264) {
        out.push(StreamCodec::H264);
    }
    if caps.hardware_encode.contains(&StreamCodec::Av1) {
        out.push(StreamCodec::Av1);
    }
    out.push(StreamCodec::Vp9);
    out
}

/// An encoder for a recording of this size, in the most compatible codec
/// that works here. A listed codec can still refuse a size (hardware
/// encoders have minimum dimensions), so each is tried in turn.
pub fn recording_encoder(
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u64,
    keyframe_interval_frames: u32,
    screen_content: bool,
) -> Result<crate::VideoEncoder, String> {
    let mut failures = Vec::new();
    for codec in recording_codecs() {
        match crate::VideoEncoder::with_options(
            codec,
            width,
            height,
            fps,
            bitrate_bps,
            keyframe_interval_frames,
            screen_content,
        ) {
            Ok(encoder) => return Ok(encoder),
            Err(error) => failures.push(format!("{}: {error}", codec.wire_label())),
        }
    }
    Err(format!(
        "no recording encoder for {width}x{height}: {}",
        failures.join("; ")
    ))
}

struct Muxer {
    pipeline: gst::Pipeline,
    video: gst_app::AppSrc,
    audio: Vec<gst_app::AppSrc>,
}

pub struct Mp4Recording {
    path: PathBuf,
    codec: StreamCodec,
    width: u32,
    height: u32,
    fps: u32,
    muxer: Option<Muxer>,
    frame_index: u64,
    audio_track_names: Vec<String>,
    audio_sample_indices: Vec<u64>,
}

impl Mp4Recording {
    pub fn new(
        path: PathBuf,
        codec: StreamCodec,
        width: u32,
        height: u32,
        fps: u32,
        audio_track_names: Vec<String>,
    ) -> Self {
        let audio_sample_indices = vec![0; audio_track_names.len()];
        Self {
            path,
            codec,
            width,
            height,
            fps: fps.max(1),
            muxer: None,
            frame_index: 0,
            audio_track_names,
            audio_sample_indices,
        }
    }

    /// The file extension this recording is written with.
    pub fn extension(with_audio: bool) -> &'static str {
        if with_audio { "mov" } else { "mp4" }
    }

    pub fn write_access_unit(&mut self, unit: &[u8]) -> Result<(), String> {
        if unit.is_empty() {
            return Ok(());
        }
        if self.muxer.is_none() {
            // A recording starts at a random-access point; anything before
            // the encoder's first keyframe could not be decoded.
            if !media::realtime::is_keyframe(self.codec, unit) {
                return Ok(());
            }
            self.muxer = Some(open_muxer(
                &self.path,
                self.codec,
                self.width,
                self.height,
                self.fps,
                &self.audio_track_names,
            )?);
        }
        let muxer = self
            .muxer
            .as_ref()
            .ok_or_else(|| "recording muxer was not initialized".to_string())?;
        let pts = frame_time(self.frame_index, u64::from(self.fps));
        let duration = frame_time(1, u64::from(self.fps));
        let mut buffer = gst::Buffer::from_slice(unit.to_vec());
        if let Some(b) = buffer.get_mut() {
            b.set_pts(pts);
            b.set_duration(duration);
            if !media::realtime::is_keyframe(self.codec, unit) {
                b.set_flags(gst::BufferFlags::DELTA_UNIT);
            }
        }
        muxer
            .video
            .push_buffer(buffer)
            .map_err(|error| format!("mux video sample: {error:?}"))?;
        self.frame_index = self.frame_index.saturating_add(1);
        Ok(())
    }

    /// Append interleaved stereo S16LE samples (48 kHz) to a named MOV audio
    /// track. Partial sample frames stay with the caller; accepting only
    /// aligned buffers keeps timestamps exact.
    pub fn write_pcm_audio(&mut self, track_index: usize, pcm: &[u8]) -> Result<(), String> {
        let sample_index = self
            .audio_sample_indices
            .get_mut(track_index)
            .ok_or_else(|| format!("audio track {track_index} was not configured"))?;
        if pcm.is_empty() {
            return Ok(());
        }
        if !pcm.len().is_multiple_of(4) {
            return Err("stereo S16 audio buffer is not sample-frame aligned".into());
        }
        let muxer = self
            .muxer
            .as_ref()
            .ok_or_else(|| "video track must start before audio is muxed".to_string())?;
        let src = muxer
            .audio
            .get(track_index)
            .ok_or_else(|| format!("audio track {track_index} has no muxer input"))?;
        let frames = u64::try_from(pcm.len() / 4).map_err(|_| "audio buffer is too large")?;
        let mut buffer = gst::Buffer::from_slice(pcm.to_vec());
        if let Some(b) = buffer.get_mut() {
            b.set_pts(frame_time(*sample_index, 48_000));
            b.set_duration(frame_time(frames, 48_000));
        }
        src.push_buffer(buffer)
            .map_err(|error| format!("mux audio: {error:?}"))?;
        *sample_index = sample_index.saturating_add(frames);
        Ok(())
    }

    /// End every track and wait for the muxer to write the trailer.
    /// # Errors
    /// Returns an error if nothing was recorded or the trailer cannot be
    /// written.
    pub fn finish(mut self) -> Result<(), String> {
        let Some(muxer) = self.muxer.take() else {
            return Err("recording ended before its first encoded frame".into());
        };
        let _ = muxer.video.end_of_stream();
        for src in &muxer.audio {
            let _ = src.end_of_stream();
        }
        let bus = muxer
            .pipeline
            .bus()
            .ok_or("recording pipeline has no bus")?;
        let msg = bus.timed_pop_filtered(
            gst::ClockTime::from_nseconds(
                u64::try_from(FINISH_TIMEOUT.as_nanos()).unwrap_or(u64::MAX),
            ),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let _ = muxer.pipeline.set_state(gst::State::Null);
        match msg.as_deref().map(gst::MessageRef::view) {
            Some(gst::MessageView::Eos(_)) => Ok(()),
            Some(gst::MessageView::Error(e)) => {
                Err(format!("finalize {}: {}", self.path.display(), e.error()))
            }
            _ => Err(format!(
                "finalize {}: the muxer did not finish within {FINISH_TIMEOUT:?}",
                self.path.display()
            )),
        }
    }
}

fn frame_time(index: u64, rate: u64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(
        u128::from(index)
            .saturating_mul(1_000_000_000)
            .checked_div(u128::from(rate.max(1)))
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(u64::MAX),
    )
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|error| format!("{factory}: {error}"))
}

fn open_muxer(
    path: &Path,
    codec: StreamCodec,
    width: u32,
    height: u32,
    fps: u32,
    audio_track_names: &[String],
) -> Result<Muxer, String> {
    gst::init().map_err(|error| format!("initialize media muxer: {error}"))?;
    let pipeline = gst::Pipeline::new();
    let mut caps = gst::Caps::from_str(codec.stream_caps())
        .map_err(|error| format!("stream caps: {error}"))?;
    if let Some(caps) = caps.get_mut() {
        caps.set(
            "width",
            i32::try_from(width).map_err(|_| "video width is too large")?,
        );
        caps.set(
            "height",
            i32::try_from(height).map_err(|_| "video height is too large")?,
        );
        caps.set(
            "framerate",
            gst::Fraction::new(i32::try_from(fps).map_err(|_| "FPS is too large")?, 1),
        );
    }
    let video = gst_app::AppSrc::builder()
        .caps(&caps)
        .format(gst::Format::Time)
        .build();
    let parser = make(codec.parser())?;
    // MOV carries raw PCM; MP4 does not, so audio selects `qtmux`.
    let mux = make(if audio_track_names.is_empty() {
        "mp4mux"
    } else {
        "qtmux"
    })?;
    let sink = gst::ElementFactory::make("filesink")
        .property(
            "location",
            path.to_str()
                .ok_or_else(|| format!("{} is not UTF-8", path.display()))?,
        )
        .build()
        .map_err(|error| format!("filesink: {error}"))?;
    let video_el = video.clone().upcast::<gst::Element>();
    pipeline
        .add_many([&video_el, &parser, &mux, &sink])
        .map_err(|error| format!("assemble muxer: {error}"))?;
    gst::Element::link_many([&video_el, &parser, &mux])
        .and_then(|()| mux.link(&sink))
        .map_err(|error| format!("link muxer: {error}"))?;

    let pcm = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field("channels", 2i32)
        .field("rate", 48_000i32)
        .build();
    let mut audio = Vec::with_capacity(audio_track_names.len());
    for name in audio_track_names {
        let src = gst_app::AppSrc::builder()
            .caps(&pcm)
            .format(gst::Format::Time)
            .build();
        let el = src.clone().upcast::<gst::Element>();
        pipeline
            .add(&el)
            .map_err(|error| format!("add audio track: {error}"))?;
        el.link(&mux)
            .map_err(|error| format!("link audio track {name}: {error}"))?;
        // The track's name travels as its title tag.
        let mut tags = gst::TagList::new();
        if let Some(t) = tags.get_mut() {
            t.add::<gst::tags::Title>(&name.as_str(), gst::TagMergeMode::Replace);
        }
        let _ = src.send_event(gst::event::Tag::new(tags));
        audio.push(src);
    }
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|error| format!("start muxer for {}: {error}", path.display()))?;
    Ok(Muxer {
        pipeline,
        video,
        audio,
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    reason = "test media durations are tiny and exactly representable in f64"
)]
mod tests {
    use super::*;
    use crate::VideoEncoder;

    /// What a demuxer sees in `path`: (video caps name, video frames,
    /// duration in seconds, audio track count).
    fn inspect(path: &Path) -> (String, u64, f64, usize) {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(&format!(
            "filesrc location=\"{}\" ! qtdemux name=d d.video_0 ! queue ! fakesink name=v sync=false",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sink = pipeline.by_name("v").unwrap();
        let pad = sink.static_pad("sink").unwrap();
        let counter = frames.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
        let demux = pipeline.by_name("d").unwrap();
        let audio_pads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let audio_count = audio_pads.clone();
        demux.connect_pad_added(move |_, pad| {
            if pad.name().starts_with("audio") {
                audio_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        pipeline.set_state(gst::State::Playing).unwrap();
        let bus = pipeline.bus().unwrap();
        let msg = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(20),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let caps = pad
            .current_caps()
            .map(|c| c.structure(0).unwrap().name().to_string())
            .unwrap_or_default();
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .map_or(0.0, |d| d.nseconds() as f64 / 1e9);
        let _ = pipeline.set_state(gst::State::Null);
        assert!(
            matches!(
                msg.as_deref().map(gst::MessageRef::view),
                Some(gst::MessageView::Eos(_))
            ),
            "demux did not finish: {msg:?}"
        );
        (
            caps,
            frames.load(std::sync::atomic::Ordering::Relaxed),
            duration,
            audio_pads.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    fn write(recording: &mut Mp4Recording, units: Vec<media::EncodedUnit>) {
        for unit in units {
            recording.write_access_unit(&unit.data).unwrap();
        }
    }

    fn frame(n: u8) -> Vec<u8> {
        let mut rgba = vec![0u8; 64 * 48 * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[n.saturating_mul(35), 110, 210u8.saturating_sub(n), 255]);
        }
        rgba
    }

    #[test]
    fn mp4_recording_starts_at_a_keyframe_and_demuxes() {
        // Phase `encoded_units_finish_as_a_demuxable_mp4`.
        // VP9 (royalty-free, software) so this runs without a GPU and without
        // Cisco's binary.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("capture.mp4");
            let mut encoder = VideoEncoder::new(StreamCodec::Vp9, 64, 48, 30).unwrap();
            let mut recording =
                Mp4Recording::new(path.clone(), StreamCodec::Vp9, 64, 48, 30, Vec::new());
            for n in 0..5u8 {
                write(&mut recording, encoder.encode(&frame(n)).unwrap());
            }
            write(&mut recording, encoder.finish().unwrap());
            recording.finish().unwrap();

            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[4..8], b"ftyp");
            let (caps, frames, duration, audio) = inspect(&path);
            assert_eq!(caps, "video/x-vp9");
            assert_eq!(frames, 5);
            assert!((0.16..0.18).contains(&duration), "{duration}");
            assert_eq!(audio, 0);
        }
        // Phase `a_recording_starts_at_the_first_keyframe`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("recent.mp4");
            let mut encoder = VideoEncoder::new(StreamCodec::Vp9, 64, 48, 30).unwrap();
            let mut units = Vec::new();
            for n in 0..6u8 {
                if n == 3 {
                    encoder.force_keyframe();
                }
                units.extend(encoder.encode(&frame(n)).unwrap());
            }
            units.extend(encoder.finish().unwrap());
            assert_eq!(units.len(), 6, "one unit per frame");
            let mut recording =
                Mp4Recording::new(path.clone(), StreamCodec::Vp9, 64, 48, 30, Vec::new());
            // Join mid-stream: the two delta frames before the keyframe are skipped.
            write(&mut recording, units.split_off(1));
            recording.finish().unwrap();
            let (_, frames, _, _) = inspect(&path);
            assert_eq!(frames, 3);
        }
    }

    #[test]
    fn mov_keeps_lossless_tracks_and_audio_follows_aligned_video() {
        // Phase `mov_keeps_lossless_sources_as_synchronized_tracks`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("capture.mov");
            let mut encoder = VideoEncoder::new(StreamCodec::Vp9, 64, 48, 30).unwrap();
            let mut recording = Mp4Recording::new(
                path.clone(),
                StreamCodec::Vp9,
                64,
                48,
                30,
                vec!["Desktop sound".into(), "Microphone".into()],
            );
            for n in 0..5u8 {
                write(&mut recording, encoder.encode(&frame(n)).unwrap());
                // 1/30 second of 48 kHz stereo S16LE.
                recording
                    .write_pcm_audio(0, &vec![0x11; 1_600 * 4])
                    .unwrap();
                recording
                    .write_pcm_audio(1, &vec![0x22; 1_600 * 4])
                    .unwrap();
            }
            write(&mut recording, encoder.finish().unwrap());
            recording.finish().unwrap();
            let (_, frames, _, audio) = inspect(&path);
            assert_eq!(frames, 5);
            assert_eq!(audio, 2, "both audio tracks are in the file");
        }
        // Phase `audio_needs_the_video_track_first_and_aligned_samples`.
        {
            let mut recording = Mp4Recording::new(
                PathBuf::from("unused.mov"),
                StreamCodec::Vp9,
                64,
                48,
                30,
                vec!["Microphone".into()],
            );
            assert!(recording.write_pcm_audio(0, &[0; 4]).is_err());
            assert!(recording.write_pcm_audio(1, &[0; 4]).is_err());
            assert!(recording.write_pcm_audio(0, &[0; 3]).is_err());
            assert!(recording.finish().is_err(), "nothing was recorded");
        }
    }
}
