//! The timeline's audio: which clips are audible at an instant, and what
//! the mixed signal is.
//!
//! Audio was the editor's largest silence. `Clip::volume` and `Track::muted`
//! existed in the CRDT and in the inspector but reached no audio path at all,
//! and export was a single video `appsrc` with no audio branch — so every
//! audio track in a project was discarded without a word.
//!
//! This module is the mixing half, and it is deliberately pure: given the
//! project and a [`PcmSource`] that can hand back samples for a media, it
//! produces the interleaved block a muxer (or a monitor) wants. That keeps
//! the semantics — per-clip volume, the fade envelope, track mute and solo,
//! and summing overlapping clips — testable without a decoder.
//!
//! It mirrors [`crate::preview::plan_layers`] on purpose: the same visibility
//! rules, expressed for sound. A track that is muted contributes nothing; a
//! soloed track silences its unsoloed siblings.

use crate::model::{ClipId, MediaId, Project, TrackKind};

/// Everything mixes at one rate, so summing never has to resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Stereo throughout. Mono sources are duplicated into both channels by the
/// [`PcmSource`], so the mixer only ever sees interleaved pairs.
pub const CHANNELS: usize = 2;

/// Where samples come from. Injected so the mixer can be exercised without a
/// decoder, exactly as the video compositor takes its textures.
pub trait PcmSource {
    /// `frames` interleaved stereo frames starting `from_secs` into `media`.
    ///
    /// Returns `None` when the media cannot be read (a reference whose bytes
    /// have not hydrated, or a file with no audio stream) — that clip then
    /// contributes silence rather than failing the mix. Short reads past the
    /// end of a source are padded with silence by the caller.
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>>;
}

/// One audible clip at an instant, with the gain it contributes at.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioLayer {
    pub clip: ClipId,
    pub media: MediaId,
    /// Seconds into the source this clip is playing.
    pub source_time: f64,
    /// Clip volume times the fade envelope, 0..1.
    pub gain: f64,
}

/// Whether `kind` has any soloed track in this project.
fn soloing(project: &Project, kind: TrackKind) -> bool {
    project.tracks.iter().any(|t| t.kind == kind && t.solo)
}

/// The audio clips audible at timeline time `t`.
///
/// Mute wins over solo, matching the picture: a muted track stays silent even
/// when it is one of the soloed ones.
pub fn plan_audio(project: &Project, t: f64) -> Vec<AudioLayer> {
    let soloed = soloing(project, TrackKind::Audio);
    let mut out = Vec::new();
    for clip in &project.clips {
        let Some(track) = project.track(clip.track) else { continue };
        if track.kind != TrackKind::Audio || track.muted || (soloed && !track.solo) {
            continue;
        }
        if !clip.covers(t) {
            continue;
        }
        let gain = (clip.volume * clip.envelope(t)).clamp(0.0, 1.0);
        out.push(AudioLayer {
            clip: clip.id,
            media: clip.media,
            source_time: crate::preview::source_time_for(clip, t),
            gain,
        });
    }
    out
}

/// Whether this project has anything to mix at all. An export asks first, so
/// a video-only timeline is muxed without an empty audio track.
pub fn has_audio(project: &Project) -> bool {
    project.clips.iter().any(|c| {
        project
            .track(c.track)
            .is_some_and(|t| t.kind == TrackKind::Audio)
    })
}

/// The peak absolute sample in an interleaved block, for a level meter.
pub fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Mix `frames` interleaved stereo frames starting at timeline second
/// `start`.
///
/// Overlapping clips sum, which is what a mix *is* — two clips playing at
/// once are heard at once. The sum is then clamped to the representable
/// range so a loud overlap distorts rather than wrapping around into noise,
/// which is what a naive cast would do.
///
/// Gain is evaluated per block rather than per sample: at the block sizes an
/// export uses (a video frame's worth, ~20ms) a fade moves by well under a
/// decibel across one, and per-sample evaluation would mean re-walking the
/// clip list 48,000 times a second for no audible difference.
pub fn mix_block(
    project: &Project,
    start: f64,
    frames: usize,
    source: &mut dyn PcmSource,
) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * CHANNELS];
    if frames == 0 {
        return out;
    }
    // Gains are read at the middle of the block so a ramp is centred on it
    // rather than lagging or leading by a whole block.
    let mid = start + (frames as f64 / SAMPLE_RATE as f64) * 0.5;
    for layer in plan_audio(project, mid) {
        if layer.gain <= 0.0 {
            continue;
        }
        let Some(samples) = source.samples(layer.media, layer.source_time, frames) else {
            continue;
        };
        let g = layer.gain as f32;
        // A short read means the source ran out; the rest of the block stays
        // silent rather than repeating whatever was last there.
        for (o, s) in out.iter_mut().zip(samples.iter()) {
            *o += s * g;
        }
    }
    for s in out.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
    out
}

/// Interleaved `f32` in -1..1 as little-endian signed 16-bit, which is what
/// the export's audio `appsrc` is fed.
pub fn to_s16le(block: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(block.len() * 2);
    for s in block {
        // 32767 rather than 32768: scaling by the negative limit lets +1.0
        // overflow to -32768, a full-scale click at the loudest moment.
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}


/// A [`PcmSource`] that decodes the project's media for real.
///
/// Each source is decoded once, in full, to interleaved stereo `f32` at
/// [`SAMPLE_RATE`] and kept in memory. A minute of stereo audio is about
/// 23 MB, which is cheap next to re-running a decoder for every 20ms block an
/// export asks for — and an export asks for every block of every clip, in
/// order, so a cache that holds the whole thing never misses twice.
#[cfg(feature = "gst")]
pub struct DecodedPcm {
    paths: std::collections::HashMap<MediaId, String>,
    loaded: std::collections::HashMap<MediaId, Option<std::sync::Arc<Vec<f32>>>>,
}

#[cfg(feature = "gst")]
impl DecodedPcm {
    /// Index every media in `project` whose bytes are readable. A reference
    /// that has not hydrated is simply absent, and its clips mix as silence.
    pub fn new(project: &Project) -> Self {
        let mut paths = std::collections::HashMap::new();
        for item in &project.media {
            let resolved = item.source();
            if !resolved.is_decodable() {
                continue;
            }
            paths.insert(
                item.id,
                crate::media::engine_source(&resolved.abs_path).to_uri(),
            );
        }
        Self { paths, loaded: std::collections::HashMap::new() }
    }

    /// How many media this source could read audio from.
    pub fn readable(&self) -> usize {
        self.paths.len()
    }

    fn pcm_for(&mut self, media: MediaId) -> Option<std::sync::Arc<Vec<f32>>> {
        if let Some(hit) = self.loaded.get(&media) {
            return hit.clone();
        }
        let decoded = self
            .paths
            .get(&media)
            .and_then(|uri| decode_pcm(uri))
            .map(std::sync::Arc::new);
        self.loaded.insert(media, decoded.clone());
        decoded
    }
}

#[cfg(feature = "gst")]
impl PcmSource for DecodedPcm {
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>> {
        let pcm = self.pcm_for(media)?;
        let start = (from_secs.max(0.0) * SAMPLE_RATE as f64) as usize * CHANNELS;
        if start >= pcm.len() {
            return None;
        }
        let end = (start + frames * CHANNELS).min(pcm.len());
        Some(pcm[start..end].to_vec())
    }
}

/// [`decode_pcm`] for tests that need to read audio back out of a file the
/// export wrote. Same decode the mixer uses, so a stream it cannot read is a
/// stream the editor cannot read either.
#[cfg(all(test, feature = "gst"))]
pub fn decode_pcm_for_test(uri: &str) -> Option<Vec<f32>> {
    decode_pcm(uri)
}

/// Decode a whole source to interleaved stereo `f32` at [`SAMPLE_RATE`].
///
/// `audioconvert ! audioresample` does the format and rate work, so the
/// result is directly mixable whatever the file held. `None` when the source
/// has no audio stream at all — a silent video is not an error.
#[cfg(feature = "gst")]
fn decode_pcm(uri: &str) -> Option<Vec<f32>> {
    use media::gst;
    use media::gst::prelude::*;
    use gstreamer_app as gst_app;

    gst::init().ok()?;
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
                .field("channels", CHANNELS as i32)
                .field("rate", SAMPLE_RATE as i32)
                .field("layout", "interleaved")
                .build(),
        )
        .sync(false)
        .build();
    pipeline
        .add_many([&src, &convert, &resample, sink.upcast_ref()])
        .ok()?;
    gst::Element::link_many([&convert, &resample, sink.upcast_ref()]).ok()?;

    // `uridecodebin` exposes pads once it knows the streams; take the audio
    // one and leave video alone.
    let convert_weak = convert.downgrade();
    src.connect_pad_added(move |_, pad| {
        let is_audio = pad
            .current_caps()
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if !is_audio {
            return;
        }
        if let Some(c) = convert_weak.upgrade() {
            if let Some(target) = c.static_pad("sink") {
                if !target.is_linked() {
                    let _ = pad.link(&target);
                }
            }
        }
    });

    if pipeline.set_state(gst::State::Playing).is_err() {
        return None;
    }
    let mut out: Vec<f32> = Vec::new();
    // A wedged pipeline must not hang the caller forever. Long enough that a
    // slow decode of a long source still finishes, short enough that a stall
    // surfaces as missing audio rather than a frozen export.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if std::time::Instant::now() >= deadline {
            tracing::warn!("audio: decoding {uri} timed out; mixing it as silence");
            break;
        }
        match sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
            Some(sample) => {
                let Some(buffer) = sample.buffer() else { continue };
                let Ok(map) = buffer.map_readable() else { continue };
                for chunk in map.chunks_exact(4) {
                    out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            None => {
                if sink.is_eos() {
                    break;
                }
                // A source with no audio stream never links the sink, so the
                // pull times out forever. Nothing pulled by the time the
                // pipeline settled means there is no audio here.
                if out.is_empty() {
                    break;
                }
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind, Track, TrackKind};
    use uuid::Uuid;

    /// A source that hands back a constant value per media, so what arrives
    /// in the mix is traceable to the clip that contributed it.
    struct Constant(std::collections::HashMap<MediaId, f32>);

    impl PcmSource for Constant {
        fn samples(&mut self, media: MediaId, _from: f64, frames: usize) -> Option<Vec<f32>> {
            let v = *self.0.get(&media)?;
            Some(vec![v; frames * CHANNELS])
        }
    }

    fn project_with_audio() -> (Project, Uuid, Uuid, Uuid) {
        let mut p = Project::new("a");
        let add_media = |p: &mut Project| {
            let id = Uuid::new_v4();
            p.media.push(MediaItem {
                id,
                name: "a".into(),
                kind: MediaKind::Audio,
                duration: 30.0,
                width: 0,
                height: 0,
                fps: 0.0,
                palette: 0,
                path: crate::media::workspace_path("a.wav"),
                hydration: crate::media::Hydration::Projected,
            });
            id
        };
        let m1 = add_media(&mut p);
        let m2 = add_media(&mut p);
        let add_track = |p: &mut Project, index: u32| {
            let id = Uuid::new_v4();
            p.tracks.push(Track {
                id,
                name: format!("A{index}"),
                kind: TrackKind::Audio,
                index,
                muted: false,
                locked: false,
                solo: false,
            });
            id
        };
        let a1 = add_track(&mut p, 1);
        let a2 = add_track(&mut p, 2);
        let c1 = p.add_clip(a1, m1, 0.0).unwrap();
        p.clip_mut(c1).unwrap().out_point = 10.0;
        let c2 = p.add_clip(a2, m2, 0.0).unwrap();
        p.clip_mut(c2).unwrap().out_point = 10.0;
        (p, a1, a2, m1)
    }

    fn constant(pairs: &[(MediaId, f32)]) -> Constant {
        Constant(pairs.iter().copied().collect())
    }

    #[test]
    fn overlapping_clips_sum() {
        let (p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let mut src = constant(&[(m1, 0.2), (m2, 0.3)]);
        let block = mix_block(&p, 1.0, 64, &mut src);
        assert_eq!(block.len(), 64 * CHANNELS);
        assert!(
            (block[0] - 0.5).abs() < 1e-6,
            "two clips playing at once are heard at once: {}",
            block[0],
        );
    }

    #[test]
    fn clip_volume_scales_what_it_contributes() {
        let (mut p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let c1 = p.clips[0].id;
        p.clip_mut(c1).unwrap().volume = 0.5;
        let mut src = constant(&[(m1, 0.4), (m2, 0.0)]);
        let block = mix_block(&p, 1.0, 16, &mut src);
        assert!((block[0] - 0.2).abs() < 1e-6, "half volume halves it: {}", block[0]);
    }

    #[test]
    fn the_fade_envelope_reaches_the_mix() {
        let (mut p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        p.clip_mut(p.clips[0].id).unwrap().fade_in = 4.0;
        let mut src = constant(&[(m1, 1.0), (m2, 0.0)]);
        let head = mix_block(&p, 0.0, 16, &mut src)[0];
        let mid = mix_block(&p, 2.0, 16, &mut src)[0];
        let after = mix_block(&p, 6.0, 16, &mut src)[0];
        assert!(head < mid && mid < after, "the ramp climbs: {head} {mid} {after}");
        assert!((after - 1.0).abs() < 1e-6, "and reaches full past the fade");
    }

    #[test]
    fn a_muted_track_is_silent_and_solo_silences_the_others() {
        let (mut p, a1, a2, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let mut src = constant(&[(m1, 0.25), (m2, 0.5)]);
        assert!((mix_block(&p, 1.0, 16, &mut src)[0] - 0.75).abs() < 1e-6);

        let i1 = p.tracks.iter().position(|t| t.id == a1).unwrap();
        let i2 = p.tracks.iter().position(|t| t.id == a2).unwrap();
        p.tracks[i1].muted = true;
        assert!(
            (mix_block(&p, 1.0, 16, &mut src)[0] - 0.5).abs() < 1e-6,
            "a muted track contributes nothing",
        );

        p.tracks[i1].muted = false;
        p.tracks[i1].solo = true;
        assert!(
            (mix_block(&p, 1.0, 16, &mut src)[0] - 0.25).abs() < 1e-6,
            "soloing one track silences the other",
        );

        // Mute still wins over solo, matching the picture.
        p.tracks[i2].solo = true;
        p.tracks[i2].muted = true;
        assert!(
            (mix_block(&p, 1.0, 16, &mut src)[0] - 0.25).abs() < 1e-6,
            "a muted track stays silent even when soloed",
        );
    }

    #[test]
    fn a_video_track_never_contributes_to_the_mix() {
        let (mut p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let idx = p.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        p.tracks[idx].kind = TrackKind::Video;
        let mut src = constant(&[(m1, 0.25), (m2, 0.5)]);
        assert!(
            (mix_block(&p, 1.0, 16, &mut src)[0] - 0.5).abs() < 1e-6,
            "only audio lanes are mixed",
        );
    }

    #[test]
    fn a_loud_overlap_clamps_instead_of_wrapping() {
        // Summed peaks above full scale must saturate. A plain cast would
        // wrap +1.2 round to a large negative sample — an audible crack at
        // exactly the loudest moment.
        let (p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let mut src = constant(&[(m1, 0.8), (m2, 0.8)]);
        let block = mix_block(&p, 1.0, 8, &mut src);
        assert!(
            block.iter().all(|s| (-1.0..=1.0).contains(s)),
            "the mix stays in range: {block:?}",
        );
        assert!((block[0] - 1.0).abs() < 1e-6, "and saturates at full scale");
        let pcm = to_s16le(&block);
        let first = i16::from_le_bytes([pcm[0], pcm[1]]);
        assert!(first > 32_000, "full scale encodes as a large positive: {first}");
    }

    #[test]
    fn full_scale_negative_does_not_overflow() {
        let (p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let mut src = constant(&[(m1, -1.0), (m2, 0.0)]);
        let pcm = to_s16le(&mix_block(&p, 1.0, 4, &mut src));
        let v = i16::from_le_bytes([pcm[0], pcm[1]]);
        assert_eq!(v, -32767, "the negative limit is representable, not wrapped");
    }

    #[test]
    fn a_source_that_yields_nothing_contributes_silence() {
        // An un-hydrated reference must not fail the mix — the rest of the
        // timeline is still audible.
        let (p, _, _, _) = project_with_audio();
        let m2 = p.media[1].id;
        let mut src = constant(&[(m2, 0.4)]); // media 1 absent
        let block = mix_block(&p, 1.0, 16, &mut src);
        assert!((block[0] - 0.4).abs() < 1e-6, "the readable clip still plays: {}", block[0]);
    }

    #[test]
    fn a_short_read_leaves_the_rest_of_the_block_silent() {
        struct Short;
        impl PcmSource for Short {
            fn samples(&mut self, _m: MediaId, _f: f64, _frames: usize) -> Option<Vec<f32>> {
                // Two frames' worth, however many were asked for.
                Some(vec![0.5; 2 * CHANNELS])
            }
        }
        let (p, _, _, _) = project_with_audio();
        let block = mix_block(&p, 1.0, 16, &mut Short);
        assert_eq!(block.len(), 16 * CHANNELS, "the block is still full length");
        assert!(block[0] > 0.0, "what the source gave is mixed");
        assert_eq!(block[15 * CHANNELS], 0.0, "past the end of the source is silence");
    }

    #[test]
    fn nothing_under_the_playhead_mixes_to_silence() {
        let (p, _, _, _) = project_with_audio();
        let (m1, m2) = (p.media[0].id, p.media[1].id);
        let mut src = constant(&[(m1, 1.0), (m2, 1.0)]);
        let block = mix_block(&p, 500.0, 32, &mut src);
        assert!(block.iter().all(|s| *s == 0.0), "past every clip the mix is silent");
        assert_eq!(peak(&block), 0.0);
    }

    #[test]
    fn a_clip_is_read_at_its_own_source_time() {
        let (mut p, _, _, _) = project_with_audio();
        let c1 = p.clips[0].id;
        {
            let c = p.clip_mut(c1).unwrap();
            c.in_point = 5.0;
            c.out_point = 15.0;
        }
        let layers = plan_audio(&p, 2.0);
        let l = layers.iter().find(|l| l.clip == c1).expect("clip is audible");
        assert!(
            (l.source_time - 7.0).abs() < 1e-9,
            "2s into a clip cut at 5s reads 7s of source: {l:?}",
        );
    }

    #[test]
    fn a_video_only_project_reports_no_audio() {
        let mut p = Project::new("v");
        assert!(!has_audio(&p), "an empty project has nothing to mix");
        let (with_audio, _, _, _) = project_with_audio();
        assert!(has_audio(&with_audio));
        // A project with only video lanes is video-only.
        p.tracks.push(Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
        });
        assert!(!has_audio(&p));
    }
}
