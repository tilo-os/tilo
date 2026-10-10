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

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use crate::model::{ClipId, MediaId, Project, TrackId, TrackKind};

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
    /// The lane it sits on — which is what the meter attributes its level to,
    /// so "the mix is clipping" can be narrowed to "on A1".
    pub track: TrackId,
    /// Seconds into the source this clip is playing.
    pub source_time: f64,
    /// **Clip volume × fade envelope × the lane's fader.**
    ///
    /// The first two are a 0..1 *trim*: they can only ever take away from what
    /// the source recorded, which is why a quiet clip could not be fixed at
    /// all before the lane had a fader. The third is
    /// [`Track::gain`](crate::model::Track::gain), which reaches
    /// [`MAX_GAIN_DB`](crate::model::MAX_GAIN_DB) — so this product runs
    /// `0.0 ..= 4.0` and a single loud layer can be over full scale on its
    /// own. That is deliberate and it is what the meter is for.
    pub gain: f64,
    /// The lane's pan, `-1`..`1`. Applied as
    /// [`Track::pan_gains`](crate::model::Track::pan_gains) when the block is
    /// summed, rather than folded into `gain`, because it is per channel and
    /// `gain` is not.
    pub pan: f64,
    /// Signed playback rate: how many source seconds pass per timeline
    /// second, negative when the clip is reversed. 1.0 at normal speed, and
    /// then the mixer reads the source one-for-one.
    pub rate: f64,
}

/// Whether `kind` has any soloed track in this project.
fn soloing(project: &Project, kind: TrackKind) -> bool {
    project.tracks.iter().any(|t| t.kind == kind && t.solo)
}

/// Whether this clip contributes sound.
///
/// Three cases, and the third is the one that is easy to get wrong:
///
///  * A clip on an **audio** lane always contributes. That is what the lane is.
///  * A clip on a **picture** lane contributes its source's own sound, because
///    a video file lands on a video track and the mixer used to read only from
///    audio tracks — so a talking-head clip was silent in the preview and
///    silent in the exported file, with nothing anywhere admitting it.
///    `has_audio` comes from the container, read at import, so this asks the
///    file rather than guessing from the track.
///  * …**unless** that sound has been split off onto an audio clip of its own
///    ([`Clip::sound_split`]). Reading it from both would play the shot twice,
///    at double level, and — the moment the two are slid apart — as an echo of
///    itself.
///
/// The third test asks the picture clip, not the link, and that distinction is
/// load-bearing. Unlinking a pair must not make the sound double, and linking a
/// music bed to a shot must not silence the shot; only *splitting the sound
/// off* moves where the sound comes from. `sound_split` is false in every
/// project written before linked audio existed, which is why those still play:
/// they have no audio clip, so their picture clip still carries the sound.
fn carries_sound(project: &Project, clip: &crate::model::Clip, track_kind: TrackKind) -> bool {
    if track_kind == TrackKind::Audio {
        return true;
    }
    if clip.sound_split {
        return false;
    }
    project
        .media
        .iter()
        .find(|m| m.id == clip.media)
        .is_some_and(|m| m.has_audio)
}

/// The audio clips audible at timeline time `t`.
///
/// Mute wins over solo, matching the picture: a muted track stays silent even
/// when it is one of the soloed ones.
pub fn plan_audio(project: &Project, t: f64) -> Vec<AudioLayer> {
    // Solo is asked per track kind, so soloing a music bed does not silence
    // the sound coming off the picture, and vice versa.
    let audio_soloed = soloing(project, TrackKind::Audio);
    let video_soloed = soloing(project, TrackKind::Video);
    let mut out = Vec::new();
    for clip in &project.clips {
        let Some(track) = project.track(clip.track) else {
            continue;
        };
        if !carries_sound(project, clip, track.kind) {
            continue;
        }
        // Mute and solo belong to the track the clip sits on, so muting the
        // video track silences that clip's sound too — which is what a person
        // means by muting the track their footage is on.
        let soloed = match track.kind {
            TrackKind::Audio => audio_soloed,
            TrackKind::Video => video_soloed,
        };
        if track.muted || (soloed && !track.solo) {
            continue;
        }
        if !clip.covers(t) {
            continue;
        }
        // Volume goes through the animation door, so a keyframed level is
        // just a different number here.
        let level = clip.param_at(crate::param::Param::Volume, t);
        // How the two levels compose, in one line: the clip's own volume and
        // its fade envelope are a **trim** — 0..1, never more than the source
        // gave — and the track's fader then scales whatever the clips on it
        // are doing. So turning a clip down is never undone by the lane, and
        // all the make-up gain lives in one place. The upper clamp that used
        // to be here is gone with the fader that replaces it: capping the
        // product at 1.0 is exactly what made a quiet clip unfixable.
        let gain = (level * clip.envelope(t)).clamp(0.0, 1.0) * track.gain();
        out.push(AudioLayer {
            clip: clip.id,
            media: clip.media,
            track: track.id,
            source_time: crate::preview::source_time_for(clip, t),
            gain,
            pan: track.pan,
            rate: clip.speed.signed(),
        });
    }
    out
}

/// Whether this lane can contribute sound at all.
///
/// An audio lane always can — that is what the lane is, empty or not. A
/// picture lane can only when something on it carries its source's own sound,
/// which is a fact about the files, not about the track.
///
/// What it is for: a meter that can never move is a meter that reads as a
/// broken output. The master strip already refuses to draw one on a
/// picture-only timeline; a lane needs the same refusal, or every video track
/// wears a level that is not merely at zero but *unreachable*.
pub fn track_carries_sound(project: &Project, track: TrackId) -> bool {
    let Some(t) = project.track(track) else {
        return false;
    };
    if t.kind == TrackKind::Audio {
        return true;
    }
    project
        .clips
        .iter()
        .filter(|c| c.track == track)
        .any(|c| carries_sound(project, c, t.kind))
}

/// Whether this project has anything to mix at all. An export asks first, so
/// a video-only timeline is muxed without an empty audio track.
pub fn has_audio(project: &Project) -> bool {
    project.clips.iter().any(|c| {
        project
            .track(c.track)
            .is_some_and(|t| carries_sound(project, c, t.kind))
    })
}

/// The peak absolute sample in an interleaved block, for a level meter.
pub fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The peak of one channel of an interleaved block. [`peak`] over both
/// together answers "how loud", this answers "how loud on which side", which
/// is what a stereo meter draws and what makes a pan visible on it.
pub fn channel_peak(block: &[f32], channel: usize) -> f32 {
    block
        .iter()
        .skip(channel.min(CHANNELS - 1))
        .step_by(CHANNELS)
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// What one mixed block measured, per channel.
///
/// **Before the clamp**, always. After it every overloaded block reads exactly
/// `1.0` and "just touched full scale" is indistinguishable from "three times
/// over" — which is the difference between a mix that is fine and one that is
/// ruined, and the number a person needs in order to know how far to come
/// down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlockMeter {
    /// Peak absolute sample, left then right. `1.0` is full scale.
    pub peak: [f32; 2],
}

impl BlockMeter {
    fn measure(block: &[f32]) -> Self {
        Self {
            peak: [channel_peak(block, 0), channel_peak(block, 1)],
        }
    }

    /// The louder channel.
    pub fn loudest(&self) -> f32 {
        self.peak[0].max(self.peak[1])
    }

    /// Whether this block asked for more than the format can carry, and so
    /// came out of the mixer distorted.
    pub fn over(&self) -> bool {
        self.loudest() > 1.0
    }
}

/// What a metered mix measured: the master, and each lane's own contribution
/// to it.
///
/// The per-track numbers are what let the warning say *which* lane, rather
/// than leaving a person to solo their way down the timeline looking for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixReport {
    pub master: BlockMeter,
    pub tracks: Vec<(TrackId, BlockMeter)>,
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
///
/// The clamp is still here and still silent — see [`mix_block_metered`],
/// which is the same mix with the measurement kept. Everything a person
/// *sees* about level goes through that one; this is the plain path an
/// encoder wants, and it allocates nothing extra to serve a meter nobody is
/// watching.
pub fn mix_block(
    project: &Project,
    start: f64,
    frames: usize,
    source: &mut dyn PcmSource,
) -> Vec<f32> {
    let mut out = mix_core(project, start, frames, source, None);
    clamp_block(&mut out);
    out
}

/// [`mix_block`], with what it measured on the way past.
///
/// This is the mix the *monitor* runs, because a meter that is fed by a
/// second, parallel analysis is a meter that can disagree with what you are
/// hearing. The measurement is taken from the summed block before the clamp,
/// so it costs one pass over samples that are already in cache and no decode
/// at all — the number is carried to the UI rather than recomputed there.
pub fn mix_block_metered(
    project: &Project,
    start: f64,
    frames: usize,
    source: &mut dyn PcmSource,
) -> (Vec<f32>, MixReport) {
    let mut per_track: Vec<(TrackId, Vec<f32>)> = Vec::new();
    let mut out = mix_core(project, start, frames, source, Some(&mut per_track));
    let report = MixReport {
        master: BlockMeter::measure(&out),
        tracks: per_track
            .iter()
            .map(|(id, block)| (*id, BlockMeter::measure(block)))
            .collect(),
    };
    clamp_block(&mut out);
    (out, report)
}

/// Saturate a summed block into the representable range. Split out so the
/// metered path can read the true peak *first* — the whole point of the
/// measurement is that it survives this.
fn clamp_block(out: &mut [f32]) {
    for s in out.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
}

/// The summed, **unclamped** block, and — when `per_track` is given — each
/// lane's own contribution to it.
///
/// One core so the plain and the metered mix can never drift apart about what
/// is audible. `per_track` is `None` for the encoder's path, which wants no
/// buffer it is not going to read.
fn mix_core(
    project: &Project,
    start: f64,
    frames: usize,
    source: &mut dyn PcmSource,
    mut per_track: Option<&mut Vec<(TrackId, Vec<f32>)>>,
) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * CHANNELS];
    if frames == 0 {
        return out;
    }
    // Gains are read at the middle of the block so a ramp is centred on it
    // rather than lagging or leading by a whole block.
    let mid = start + (frames as f64 / f64::from(SAMPLE_RATE)) * 0.5;
    for mut layer in plan_audio(project, mid) {
        if layer.gain <= 0.0 {
            continue;
        }
        // The *gain* belongs at the block's middle; the read position does
        // not. Planning at the middle and reading there too made every
        // block start half a block into the source it should have covered
        // — a constant offset between the sound and the picture.
        if let Some(clip) = project.clip(layer.clip) {
            layer.source_time = clip.source_time(start);
        }
        let Some(samples) = fetch_at_rate(source, &layer, frames) else {
            continue;
        };
        // Pan is per channel, so the per-sample multiplier alternates. At the
        // default centre both are the lane's plain gain and this is the same
        // arithmetic it always was.
        let (pl, pr) = project
            .track(layer.track)
            .map_or((1.0, 1.0), crate::model::Track::pan_gains);
        let g = [(layer.gain * pl) as f32, (layer.gain * pr) as f32];
        let bucket = per_track.as_deref_mut().map(|buckets| {
            if let Some(i) = buckets.iter().position(|(t, _)| *t == layer.track) {
                i
            } else {
                buckets.push((layer.track, vec![0.0f32; frames * CHANNELS]));
                buckets.len() - 1
            }
        });
        // A short read means the source ran out; the rest of the block stays
        // silent rather than repeating whatever was last there.
        for (i, s) in samples.iter().enumerate().take(out.len()) {
            let v = s * g.get(i % CHANNELS).copied().unwrap_or_default();
            if let Some(output) = out.get_mut(i) {
                *output += v;
            }
            if let (Some(idx), Some(buckets)) = (bucket, per_track.as_deref_mut())
                && let Some((_, samples)) = buckets.get_mut(idx)
                && let Some(sample) = samples.get_mut(i)
            {
                *sample += v;
            }
        }
    }
    out
}

/// Read `frames` output frames for `layer`, resampling when it does not
/// play at normal speed.
///
/// At rate 1.0 this is the plain read it always was. Otherwise it pulls the
/// span of source the block covers and resamples it, which changes the
/// pitch along with the tempo — the tape-speed behaviour, not a
/// time-stretch. That is deliberate: a pitch-preserving stretch is a real
/// signal-processing job (overlap-add or a phase vocoder), and shipping a
/// half-built one that smears transients would be worse than an honest
/// varispeed the user can hear is a varispeed.
fn fetch_at_rate(
    source: &mut dyn PcmSource,
    layer: &AudioLayer,
    frames: usize,
) -> Option<Vec<f32>> {
    let rate = layer.rate;
    if (rate - 1.0).abs() < 1e-9 {
        return source.samples(layer.media, layer.source_time, frames);
    }
    if !rate.is_finite() || rate.abs() < 1e-9 || frames == 0 {
        return None;
    }
    let span = frames as f64 * rate.abs();
    // One extra frame so the last output sample has a right-hand neighbour
    // to interpolate toward.
    let need = span.ceil() as usize + 2;
    // Reversed, the block covers the source *behind* the current position,
    // so the read starts there and the samples are walked backwards.
    let from = if rate < 0.0 {
        (layer.source_time - span / f64::from(SAMPLE_RATE)).max(0.0)
    } else {
        layer.source_time
    };
    let src = source.samples(layer.media, from, need)?;
    let have = src.len() / CHANNELS;
    if have == 0 {
        return None;
    }
    let mut out = vec![0.0f32; frames * CHANNELS];
    let (src_frames, _) = src.as_chunks::<CHANNELS>();
    let (out_frames, _) = out.as_chunks_mut::<CHANNELS>();
    for (i, out_frame) in out_frames.iter_mut().enumerate() {
        // Where this output frame falls in the source block.
        //
        // Reversed, the block covers `(source_time - span, source_time]`,
        // so its first output frame is the *last sample before* the source
        // time — not the sample at it, which for a clip whose out-point is
        // the end of the media is one frame past anything that exists.
        let pos = if rate < 0.0 {
            span - ((i as f64 + 1.0) * rate.abs())
        } else {
            i as f64 * rate
        };
        if pos < 0.0 {
            continue;
        }
        let j = pos.floor() as usize;
        let Some(left) = src_frames.get(j) else {
            // Past what the source gave: the rest of the block is silent
            // rather than a repeat of the last frame.
            break;
        };
        let frac = (pos - j as f64) as f32;
        // The window's final sample has no right-hand neighbour to
        // interpolate toward. Hold it for that one frame rather than
        // ramping into a zero the source never contained, which would
        // put a click on the edge of every block that lands there.
        let right = src_frames.get(j + 1).unwrap_or(left);
        for ((o, &a), &b) in out_frame.iter_mut().zip(left).zip(right) {
            *o = a + (b - a) * frac;
        }
    }
    Some(out)
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
#[cfg(any(feature = "gst", feature = "decode"))]
pub struct DecodedPcm {
    paths: std::collections::HashMap<MediaId, String>,
    loaded: std::collections::HashMap<MediaId, Option<std::sync::Arc<Vec<f32>>>>,
}

#[cfg(any(feature = "gst", feature = "decode"))]
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
            match crate::media::engine_source(&resolved.abs_path).to_uri() {
                Ok(uri) => {
                    paths.insert(item.id, uri);
                }
                Err(error) => tracing::warn!(%error, "media path cannot be decoded"),
            }
        }
        Self {
            paths,
            loaded: std::collections::HashMap::new(),
        }
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

#[cfg(any(feature = "gst", feature = "decode"))]
impl PcmSource for DecodedPcm {
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>> {
        let pcm = self.pcm_for(media)?;
        let start = (from_secs.max(0.0) * f64::from(SAMPLE_RATE)) as usize * CHANNELS;
        if start >= pcm.len() {
            return None;
        }
        let end = (start + frames * CHANNELS).min(pcm.len());
        pcm.get(start..end).map(<[f32]>::to_vec)
    }
}

/// [`decode_pcm`] for tests that need to read audio back out of a file the
/// export wrote. Same decode the mixer uses, so a stream it cannot read is a
/// stream the editor cannot read either.
#[cfg(test)]
#[cfg(any(feature = "gst", feature = "decode"))]
pub fn decode_pcm_for_test(uri: &str) -> Option<Vec<f32>> {
    decode_pcm(uri)
}

/// Whether a pull that came back empty means the source has no audio at all.
///
/// It means that only once the demuxer has finished exposing its pads and
/// none of them was audio. On its own an empty pull proves nothing about the
/// file: `uridecodebin` has to typefind and autoplug before anything reaches
/// the sink, and on a busy machine that takes longer than one pull's timeout.
///
/// Treating the first quiet pull as proof is how a real soundtrack got mixed
/// as silence whenever the machine was loaded -- and, in the test suite, how
/// an export that had worked was reported as one that had produced nothing.
#[cfg(any(feature = "gst", feature = "decode"))]
fn no_audio_concluded(pads_settled: bool, audio_linked: bool) -> bool {
    pads_settled && !audio_linked
}

/// Decode a whole source to interleaved stereo `f32` at [`SAMPLE_RATE`].
///
/// `audioconvert ! audioresample` does the format and rate work, so the
/// result is directly mixable whatever the file held. `None` when the source
/// has no audio stream at all — a silent video is not an error.
#[cfg(any(feature = "gst", feature = "decode"))]
fn decode_pcm(uri: &str) -> Option<Vec<f32>> {
    use gstreamer_app as gst_app;
    use media::gst;
    use media::gst::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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
    let linked = Arc::new(AtomicBool::new(false));
    let convert_weak = convert.downgrade();
    let linked_in = linked.clone();
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
    let settled_in = settled.clone();
    src.connect_no_more_pads(move |_| settled_in.store(true, Ordering::SeqCst));

    let bus = pipeline.bus();
    if pipeline.set_state(gst::State::Playing).is_err() {
        return None;
    }
    let mut out: Vec<f32> = Vec::new();
    // A wedged pipeline must not hang the caller forever. Long enough that a
    // slow decode of a long source still finishes, short enough that a stall
    // surfaces as missing audio rather than a frozen export.
    let deadline = ui::time::now() + std::time::Duration::from_secs(120);
    loop {
        if ui::time::now() >= deadline {
            tracing::warn!("audio: decoding {uri} timed out; mixing it as silence");
            break;
        }
        // A pipeline that has failed will never produce another buffer, so
        // stop waiting on one instead of sitting out the whole deadline --
        // two minutes of waiting is indistinguishable from a hang to
        // whoever is watching.
        let mut failed = false;
        if let Some(bus) = bus.as_ref() {
            while let Some(msg) = bus.pop() {
                if let gst::MessageView::Error(e) = msg.view() {
                    tracing::warn!("audio: decoding {uri} failed: {}", e.error());
                    failed = true;
                }
            }
        }
        if failed {
            break;
        }
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
            let Some(buffer) = sample.buffer() else {
                continue;
            };
            let Ok(map) = buffer.map_readable() else {
                continue;
            };
            let (words, _) = map.as_chunks::<4>();
            out.extend(words.iter().map(|&w| f32::from_le_bytes(w)));
        } else {
            if sink.is_eos() {
                break;
            }
            // A source with no audio stream never links the sink, so the
            // pull times out forever. That is only proof of "no audio
            // here" once the demuxer has said it has no more pads to
            // offer -- see `no_audio_concluded`.
            if out.is_empty()
                && no_audio_concluded(
                    settled.load(Ordering::SeqCst),
                    linked.load(Ordering::SeqCst),
                )
            {
                break;
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------------------
// Metering — what the mix measured, kept where the UI can read it.
// ---------------------------------------------------------------------------

/// How fast a meter falls, in decibels per second.
///
/// A peak meter rises instantly and falls slowly: the rise is the event you
/// are trying to see and the fall is only there so the next one is legible.
/// 24 dB/s is between the broadcast PPM (about 12) and the near-instant fall
/// of a DAW meter — slow enough that a transient is still on screen when your
/// eye gets to it, fast enough that the bar tracks a phrase rather than
/// smearing into a ceiling.
const METER_RELEASE_DB_PER_SEC: f64 = 24.0;

/// Two overloads closer together than this are one event.
///
/// Without it a second of sustained clipping is 47 separate entries, a list
/// nobody can use and a count that says "47 places" about one place.
const OVER_MERGE_SECS: f64 = 0.5;

/// How many distinct overloads are remembered. Past this the list stops
/// growing rather than the oldest being dropped: the *first* place the mix
/// went over is the one to fix, and a list that forgot it in favour of the
/// sixty-fifth would be worse than no list.
const MAX_OVERS: usize = 64;

/// One lane's measured level, as the UI draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackLevel {
    pub track: TrackId,
    /// Falling peak per channel — what the bar shows.
    pub level: [f32; 2],
    /// The loudest this lane has been since the meter was last cleared.
    pub hold: f32,
    /// **Latched.** True from the moment this lane's own contribution went
    /// past full scale until the meter is cleared, which is what turns a
    /// one-frame flash into something a person who was looking at the
    /// timeline can still find afterwards.
    pub clipped: bool,
}

/// Everything the UI needs from the meter, in one cheap copy.
///
/// Taken once per `sync` and read from plain fields during `draw`, so the
/// paint path never takes the mixing thread's lock and never allocates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixSnapshot {
    /// Falling master peak per channel.
    pub master: [f32; 2],
    /// The loudest the master has been since the last clear.
    pub master_hold: f32,
    pub tracks: Vec<TrackLevel>,
    /// The worst pre-clamp peak seen while over full scale, `0.0` when the
    /// mix has never gone over. `1.6` here means the mix asked for 60% more
    /// than the format can carry.
    pub worst: f32,
    /// Timeline seconds where the mix went over, in the order they happened.
    /// This is the answer to "where", and it is why the warning is worth
    /// having rather than merely alarming.
    pub overs: Vec<f64>,
    /// Whether anything has been measured at all since the last clear. A
    /// quiet meter and a meter nobody has fed look identical, and telling a
    /// person their levels are fine when nothing has played would be a lie.
    pub monitored: bool,
}

impl MixSnapshot {
    /// Whether the mix has gone over since the meter was last cleared.
    pub fn clipped(&self) -> bool {
        !self.overs.is_empty()
    }

    /// How far over, in decibels. `0.0` when it never went over.
    pub fn worst_over_db(&self) -> f64 {
        if self.worst <= 1.0 {
            return 0.0;
        }
        20.0 * f64::from(self.worst).log10()
    }

    /// The master peak-hold in decibels relative to full scale, or `None`
    /// when nothing has been measured.
    pub fn hold_db(&self) -> Option<f64> {
        self.monitored
            .then(|| crate::model::linear_to_db(f64::from(self.master_hold)))
    }

    /// The first overload strictly after `t`, wrapping to the first one when
    /// there is none — so pressing the warning repeatedly walks the list and
    /// then starts again, rather than parking on the last one.
    pub fn next_over_after(&self, t: f64) -> Option<f64> {
        self.overs
            .iter()
            .copied()
            .find(|o| *o > t + 1e-6)
            .or_else(|| self.overs.first().copied())
    }

    pub fn track(&self, id: TrackId) -> Option<&TrackLevel> {
        self.tracks.iter().find(|t| t.track == id)
    }

    /// Every lane that contributed to what was measured, clipped or not.
    ///
    /// The distinction from [`Self::clipped_tracks`] is the whole of what a
    /// trim has to know: the lanes to *name* in a warning are the ones caught
    /// over, and the lanes to *move* are all of them, because the mix is their
    /// sum. See [`Project::lanes_trimmed_by`](crate::model::Project::lanes_trimmed_by).
    pub fn measured_tracks(&self) -> Vec<TrackId> {
        self.tracks.iter().map(|t| t.track).collect()
    }

    /// The lanes that have clipped, loudest first — what a warning names.
    pub fn clipped_tracks(&self) -> Vec<TrackId> {
        let mut hot: Vec<&TrackLevel> = self.tracks.iter().filter(|t| t.clipped).collect();
        hot.sort_by(|a, b| b.hold.partial_cmp(&a.hold).unwrap_or(Ordering::Equal));
        hot.iter().map(|t| t.track).collect()
    }
}

#[derive(Default)]
struct MeterState {
    master: [f32; 2],
    master_hold: f32,
    tracks: Vec<TrackLevel>,
    worst: f32,
    overs: Vec<f64>,
    monitored: bool,
}

/// The level the mix is running at, shared between the thread that mixes and
/// the one that draws.
///
/// It exists because [`peak`] was written "for a level meter" and had no
/// caller: the measurement was already being thrown away every block, and a
/// mix that clamped in silence was the result. Nothing here re-analyses
/// anything — the mixer hands over what it already computed.
#[derive(Default)]
pub struct MixMeters {
    state: Mutex<MeterState>,
}

impl MixMeters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take in what one block measured, at timeline second `at`.
    ///
    /// Levels only ever *rise* here; the fall is [`Self::decay`]'s job, driven
    /// by the frame clock. Splitting them is what keeps the meter's fall rate
    /// a property of time rather than of how often the mixer happened to run.
    pub fn record(&self, at: f64, report: &MixReport) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        s.monitored = true;
        for (level, peak) in s.master.iter_mut().zip(report.master.peak) {
            *level = level.max(peak);
        }
        s.master_hold = s.master_hold.max(report.master.loudest());
        for (id, m) in &report.tracks {
            let i = if let Some(i) = s.tracks.iter().position(|t| t.track == *id) {
                i
            } else {
                s.tracks.push(TrackLevel {
                    track: *id,
                    ..Default::default()
                });
                s.tracks.len() - 1
            };
            // `i` was just found or pushed.
            let Some(t) = s.tracks.get_mut(i) else {
                continue;
            };
            for (level, peak) in t.level.iter_mut().zip(m.peak) {
                *level = level.max(peak);
            }
            t.hold = t.hold.max(m.loudest());
            // Latched, never cleared by a quieter block: the point of the
            // indicator is that it outlives the moment.
            t.clipped |= m.over();
        }
        if !report.master.over() {
            return;
        }
        s.worst = s.worst.max(report.master.loudest());
        let fresh = s
            .overs
            .last()
            .is_none_or(|last| (at - last).abs() > OVER_MERGE_SECS);
        if fresh && s.overs.len() < MAX_OVERS {
            s.overs.push(at);
            s.overs
                .sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        }
    }

    /// Let the meters fall by `dt` seconds' worth. Called from the frame
    /// clock, so the bar falls at the same rate whether the mixer is running
    /// at 1024-sample blocks or not running at all.
    pub fn decay(&self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        let factor = crate::model::db_to_linear(-METER_RELEASE_DB_PER_SEC * dt.min(1.0)) as f32;
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        for level in &mut s.master {
            *level *= factor;
        }
        for t in &mut s.tracks {
            for level in &mut t.level {
                *level *= factor;
            }
        }
    }

    /// Forget what was measured: the latched overs, the holds and the worst
    /// overshoot.
    ///
    /// Called when the mix itself changes (see [`mix_signature`]), because a
    /// warning about levels the user has since altered is a warning about a
    /// mix that no longer exists — and one that never went away would train
    /// people to ignore the one that matters.
    pub fn clear(&self) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        *s = MeterState::default();
    }

    pub fn snapshot(&self) -> MixSnapshot {
        let Ok(s) = self.state.lock() else {
            return MixSnapshot::default();
        };
        MixSnapshot {
            master: s.master,
            master_hold: s.master_hold,
            tracks: s.tracks.clone(),
            worst: s.worst,
            overs: s.overs.clone(),
            monitored: s.monitored,
        }
    }
}

/// A hash of everything that decides what the mix sounds like.
///
/// Used to tell "the warning still describes this timeline" from "the user has
/// changed the levels since". Deliberately narrow: renaming a track or
/// recolouring a clip must not throw away a clipping report, and moving an
/// audible clip must.
pub fn mix_signature(project: &Project) -> u64 {
    // FNV-1a: stable across runs and machines, and this is a change detector
    // rather than a security boundary.
    let mut hasher = fnv::FnvHasher::default();
    let mut eat = |bytes: &[u8]| std::hash::Hasher::write(&mut hasher, bytes);
    for t in &project.tracks {
        eat(t.id.as_bytes());
        eat(&t.gain_db.to_bits().to_le_bytes());
        eat(&t.pan.to_bits().to_le_bytes());
        eat(&[u8::from(t.muted), u8::from(t.solo), t.kind as u8]);
    }
    for c in &project.clips {
        eat(c.id.as_bytes());
        eat(c.track.as_bytes());
        eat(c.media.as_bytes());
        for v in [
            c.start,
            c.in_point,
            c.out_point,
            c.volume,
            c.fade_in,
            c.fade_out,
            c.speed.signed(),
        ] {
            eat(&v.to_bits().to_le_bytes());
        }
        eat(&[u8::from(c.sound_split)]);
        if let Some(track) = c.anim.track(crate::param::Param::Volume) {
            for k in &track.keys {
                eat(&k.t.to_bits().to_le_bytes());
                eat(&k.v.to_bits().to_le_bytes());
            }
        }
    }
    std::hash::Hasher::finish(&hasher)
}

// ---------------------------------------------------------------------------
// Monitoring — hearing the timeline while it plays.
// ---------------------------------------------------------------------------

/// How much audio the monitor mixes per step. Small enough that a pause or a
/// scrub is acted on promptly, large enough that the mixing thread is not
/// waking thousands of times a second.
pub const MONITOR_BLOCK: usize = 1024;

/// Where monitored audio goes. Abstracted so the transport logic — what plays,
/// from where, and what happens on a seek — is testable without an audio
/// device, which no CI box has.
pub trait MonitorSink: Send {
    /// Push one block of interleaved `f32`. Returning `false` stops the
    /// monitor (the device went away).
    fn push(&mut self, block: &[f32]) -> bool;
}

/// A sink that throws audio away. What the editor uses when the machine has
/// no audio output — playback still runs, it is simply silent, which beats
/// refusing to play.
pub struct NullSink;

impl MonitorSink for NullSink {
    fn push(&mut self, _block: &[f32]) -> bool {
        true
    }
}

/// The best output this machine offers: the system sink where one opens,
/// [`NullSink`] where none does.
///
/// A box with no audio device is a normal state, not a failure — a headless
/// CI runner is one — so it plays silently rather than refusing to play. The
/// caller can tell which it got from [`output_is_audible`].
#[cfg(any(feature = "gst", feature = "decode"))]
pub fn open_output() -> Box<dyn MonitorSink> {
    if let Some(s) = SystemSink::open() {
        Box::new(s)
    } else {
        tracing::info!("no audio output available; monitoring silently");
        Box::new(NullSink)
    }
}

/// Whether this machine has an audio output at all, for an honest UI: the
/// transport can then say it is playing *silently* rather than implying
/// sound the user is not getting.
#[cfg(any(feature = "gst", feature = "decode"))]
pub fn output_is_audible() -> bool {
    SystemSink::open().is_some()
}

/// What the monitor thread reads and the UI writes.
struct MonitorState {
    project: Mutex<Project>,
    /// Where the next block is mixed from, in timeline seconds.
    position: Mutex<f64>,
    playing: AtomicBool,
    stop: AtomicBool,
}

/// Plays the mixed timeline while the transport runs.
///
/// The editor used to advance a UI clock and nothing else — there was no
/// audio output at all, so `Clip::volume` and `Track::muted` were settings
/// with nothing on the other end of them.
///
/// Monitoring here is deliberate rather than incidental: one pipeline that
/// runs only while the transport is playing. The preview's per-frame
/// seek-play-pause pattern used to fire the real audio sink for a few
/// milliseconds every frame, which is audible as a stutter — a monitor must
/// never be built out of that.
pub struct AudioMonitor {
    shared: Arc<MonitorState>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AudioMonitor {
    /// Start a monitor feeding `sink`, initially paused.
    ///
    /// `source` supplies the samples; it lives on the monitor thread, so a
    /// decode it does cannot stall the UI. `meters` receives what every block
    /// measured — the monitor is the one place in the editor where real
    /// samples and the user's attention are in the same room, so it is the
    /// one place a level meter can honestly be fed from.
    pub fn new(
        project: &Project,
        mut sink: Box<dyn MonitorSink>,
        mut source: Box<dyn PcmSource + Send>,
        meters: Arc<MixMeters>,
    ) -> Self {
        let shared = Arc::new(MonitorState {
            project: Mutex::new(project.clone()),
            position: Mutex::new(0.0),
            playing: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let state = shared.clone();
        let handle = ui::rt::spawn_thread("video-editor-monitor", move || {
            let block_secs = MONITOR_BLOCK as f64 / f64::from(SAMPLE_RATE);
            while !state.stop.load(AtomicOrdering::Relaxed) {
                if !state.playing.load(AtomicOrdering::Relaxed) {
                    // Idle cheaply. Nothing is pushed while paused, so a
                    // paused editor makes no sound at all.
                    let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(10));
                    continue;
                }
                let at = *state
                    .position
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (block, report) = {
                    let project = state
                        .project
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    mix_block_metered(&project, at, MONITOR_BLOCK, source.as_mut())
                };
                // Recorded before the push, so a device that goes away
                // mid-block still leaves behind what the last one measured
                // rather than swallowing the overload that killed it.
                meters.record(at, &report);
                if !sink.push(&block) {
                    break;
                }
                // Advance by exactly what was mixed, so the audio clock
                // is the sample count rather than wall time and cannot
                // drift away from the picture.
                *state
                    .position
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = at + block_secs;
            }
        })
        .ok();
        Self { shared, handle }
    }

    /// Replace the timeline being monitored. Call it when the project changes
    /// so an edit is heard on the next block rather than the next play.
    pub fn set_project(&self, project: &Project) {
        if let Ok(mut p) = self.shared.project.lock() {
            *p = project.clone();
        }
    }

    /// Start playing from `secs`.
    pub fn play(&self, secs: f64) {
        self.seek(secs);
        self.shared.playing.store(true, AtomicOrdering::Relaxed);
    }

    /// Stop playing, leaving the position where it is.
    pub fn pause(&self) {
        self.shared.playing.store(false, AtomicOrdering::Relaxed);
    }

    /// Move the playhead. Safe while playing — the next block comes from the
    /// new position.
    pub fn seek(&self, secs: f64) {
        if let Ok(mut p) = self.shared.position.lock() {
            *p = secs.max(0.0);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(AtomicOrdering::Relaxed)
    }

    /// Where the monitor has mixed up to, in timeline seconds.
    pub fn position(&self) -> f64 {
        self.shared.position.lock().map_or(0.0, |p| *p)
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, AtomicOrdering::Relaxed);
        self.shared.playing.store(false, AtomicOrdering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A [`MonitorSink`] that plays through the system's audio output.
///
/// `autoaudiosink` picks whatever the host offers (`PipeWire` here). A machine
/// with no output at all yields `None`, and the editor falls back to
/// [`NullSink`] — silent playback rather than no playback.
#[cfg(any(feature = "gst", feature = "decode"))]
pub struct SystemSink {
    pipeline: media::gst::Pipeline,
    src: gstreamer_app::AppSrc,
    pushed: u64,
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl SystemSink {
    pub fn open() -> Option<Self> {
        use media::gst;
        use media::gst::prelude::*;

        gst::init().ok()?;
        let pipeline = gst::Pipeline::new();
        let src = gstreamer_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("channels", CHANNELS as i32)
                    .field("rate", SAMPLE_RATE as i32)
                    .field("layout", "interleaved")
                    .build(),
            )
            // The sink's own clock paces playback; the monitor pushes ahead
            // of it and blocks when the queue is full, which is what keeps
            // the mixing thread in step without a timer of its own.
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        src.set_property("block", true);
        let convert = gst::ElementFactory::make("audioconvert").build().ok()?;
        let resample = gst::ElementFactory::make("audioresample").build().ok()?;
        let out = gst::ElementFactory::make("autoaudiosink").build().ok()?;
        pipeline
            .add_many([src.upcast_ref(), &convert, &resample, &out])
            .ok()?;
        gst::Element::link_many([src.upcast_ref(), &convert, &resample, &out]).ok()?;
        pipeline.set_state(gst::State::Playing).ok()?;
        Some(Self {
            pipeline,
            src,
            pushed: 0,
        })
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl MonitorSink for SystemSink {
    fn push(&mut self, block: &[f32]) -> bool {
        use media::gst;

        let mut bytes = Vec::with_capacity(block.len() * 4);
        for s in block {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let Ok(mut buffer) = gst::Buffer::with_size(bytes.len()) else {
            return false;
        };
        {
            let Some(bref) = buffer.get_mut() else {
                return false;
            };
            let frames = (block.len() / CHANNELS) as u64;
            let ns = |f: u64| f * 1_000_000_000 / u64::from(SAMPLE_RATE);
            bref.set_pts(gst::ClockTime::from_nseconds(ns(self.pushed)));
            bref.set_duration(gst::ClockTime::from_nseconds(ns(frames)));
            let Ok(mut map) = bref.map_writable() else {
                return false;
            };
            map.copy_from_slice(&bytes);
            self.pushed += frames;
        }
        self.src.push_buffer(buffer).is_ok()
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl Drop for SystemSink {
    fn drop(&mut self) {
        use media::gst;
        use media::gst::prelude::*;
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "decode")]
    #[allow(dead_code, reason = "only the media-stack check is used here")]
    mod generated_clip {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/generated_clip.rs"
        ));
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "silence is written as literal zeros, not computed"
    )]
    fn mixer_fills_missing_or_short_sources_with_silence() {
        // Case `a_source_that_yields_nothing_contributes_silence`.
        {
            // An un-hydrated reference must not fail the mix — the rest of the
            // timeline is still audible.
            let (p, _, _, _) = project_with_audio();
            let m2 = p.media[1].id;
            let mut src = constant(&[(m2, 0.4)]); // media 1 absent
            let block = mix_block(&p, 1.0, 16, &mut src);
            assert!(
                (block[0] - 0.4).abs() < 1e-6,
                "the readable clip still plays: {}",
                block[0]
            );
        }
        // Case `a_short_read_leaves_the_rest_of_the_block_silent`.
        {
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
            assert_eq!(
                block[15 * CHANNELS],
                0.0,
                "past the end of the source is silence"
            );
        }
        // Case `nothing_under_the_playhead_mixes_to_silence`.
        {
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let mut src = constant(&[(m1, 1.0), (m2, 1.0)]);
            let block = mix_block(&p, 500.0, 32, &mut src);
            assert!(
                block.iter().all(|s| *s == 0.0),
                "past every clip the mix is silent"
            );
            assert_eq!(peak(&block), 0.0);
        }
        // Case `quiet_before_the_demuxer_has_settled_is_not_proof_of_no_audio`:
        // A pull that came back empty used to be read as proof that the file had
        // no audio stream. It is not, and stating the rule here pins the exact
        // distinction the loop turns on.
        #[cfg(any(feature = "gst", feature = "decode"))]
        {
            use super::no_audio_concluded;
            // Nothing pulled, nothing linked, pads still arriving: an empty pull
            // here is a statement about the clock, not about the file. This is
            // the case that was getting it wrong -- and it is the one a loaded
            // machine produces, which is why it failed under a full test run and
            // passed on its own.
            assert!(!no_audio_concluded(false, false));
            // The demuxer has now said that is every pad there is, and none was
            // audio. Only now is the silence about the file.
            assert!(no_audio_concluded(true, false));
            // An audio pad IS linked, so quiet means the pipeline has not got
            // going yet -- never a reason to stop waiting for it.
            assert!(!no_audio_concluded(true, true));
            assert!(!no_audio_concluded(false, true));
        }
    }

    /// A file in the repository, so this test cannot pass by looking at
    /// nothing.
    #[cfg(feature = "decode")]
    fn sample(name: &str) -> std::path::PathBuf {
        let p = crate::media::bundled_sample_media()
            .join("media")
            .join(name);
        assert!(p.exists(), "the bundled fixture {name} is missing");
        p
    }

    #[test]
    #[cfg(feature = "decode")]
    #[serial_test::serial(video_editor_gst)]
    fn real_source_decode_settles_or_gives_up() {
        // Case `a_source_that_carries_audio_decodes_to_real_samples`:
        // The positive half of the rule: a source that really does carry audio
        // has to come back with it. A decoder that returned `None` here would
        // make every "the export is silent" assertion in this crate vacuous.
        'case: {
            if !generated_clip::media_stack() {
                break 'case;
            }
            let uri = crate::media::engine_source(&sample("Ambient_Pad.wav"))
                .to_uri()
                .expect("fixture file URL");
            let pcm = super::decode_pcm_for_test(&uri).expect("the fixture has an audio stream");
            assert!(
                pcm.len() > super::SAMPLE_RATE as usize / 4,
                "a real read, not a handful of frames: {} samples",
                pcm.len(),
            );
            assert!(super::peak(&pcm) > 0.01, "and it is not silence");
        }
        // Case `an_unreadable_source_gives_up_long_before_the_deadline`:
        // The negative half: a source the decoder cannot make sense of must be
        // given up on promptly. The loop waits two minutes before calling a
        // stalled decode silence, and two minutes is indistinguishable from a
        // hang to whoever is waiting -- so the bound is asserted, not assumed.
        {
            let junk =
                std::env::temp_dir().join(format!("tilo-ve-junk-{}.mp4", std::process::id()));
            std::fs::write(&junk, b"not a container, not even close").expect("write the fixture");
            let uri = crate::media::engine_source(&junk)
                .to_uri()
                .expect("fixture file URL");

            let began = ui::time::now();
            let got = super::decode_pcm_for_test(&uri);
            let took = began.elapsed();
            std::fs::remove_file(&junk).ok();

            assert!(
                got.is_none(),
                "there is no audio to be had in {} bytes of junk",
                31
            );
            assert!(
                took < std::time::Duration::from_secs(30),
                "gave up after {took:?}; the pipeline's failure has to end the wait",
            );
        }
    }

    /// A source whose samples encode *where they came from*: frame `n` of a
    /// media is the value `n`. A resampler's output can then be read
    /// straight back as "which source frames did this block cover", which is
    /// the only way to check a rate change without hand-waving.
    struct RampSource {
        len: usize,
    }

    impl PcmSource for RampSource {
        fn samples(&mut self, _media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>> {
            let start = (from_secs * f64::from(SAMPLE_RATE)).round() as usize;
            let mut out = Vec::with_capacity(frames * CHANNELS);
            for i in 0..frames {
                let n = start + i;
                if n >= self.len {
                    break;
                }
                // Both channels carry the frame index, normalised by the
                // source length so the ramp never leaves -1..1 and gets
                // clamped by the mixer (which would erase the very thing
                // this source exists to report).
                let v = n as f32 / self.len as f32;
                out.push(v);
                out.push(v);
            }
            if out.is_empty() { None } else { Some(out) }
        }
    }

    /// A project with one audio clip covering `[0, len)` at `rate`.
    fn speed_project(rate: f64, reverse: bool, len: f64) -> (Project, MediaId) {
        let mut p = Project::new("s");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "a.wav".into(),
            kind: MediaKind::Audio,
            duration: len,
            fps: 0.0,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        let c = p.clip_mut(cid).unwrap();
        c.out_point = len;
        c.speed = crate::model::Speed::new(rate, reverse);
        (p, mid)
    }

    /// Which source frame a block's first and last output samples came from,
    /// read back out of the ramp.
    fn span_of(block: &[f32], len: usize) -> (f64, f64) {
        let first = f64::from(block[0]) * len as f64;
        let last = f64::from(block[block.len() - CHANNELS]) * len as f64;
        (first, last)
    }

    /// A project with one audio clip placed at `start` on the timeline,
    /// sourcing from `in_point` for `len` seconds.
    ///
    /// Both offsets are non-zero on purpose. A clip at timeline 0 sourcing
    /// from 0 makes the timeline second and the source second the same
    /// number, so a mixer that confuses the two still looks right.
    fn placed_project(start: f64, in_point: f64, len: f64) -> Project {
        let mut p = Project::new("p");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "a.wav".into(),
            kind: MediaKind::Audio,
            duration: in_point + len,
            fps: 0.0,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "A1".into(),
            kind: TrackKind::Audio,
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, start).expect("media exists");
        let c = p.clip_mut(cid).unwrap();
        c.in_point = in_point;
        c.out_point = in_point + len;
        p
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn audio_blocks_read_their_span_at_the_clip_rate() {
        // Case `a_block_reads_the_span_it_covers_not_one_offset_from_it`:
        // A block must read the span of source it *covers*, not one displaced
        // from it.
        //
        // `mix_block` evaluates gain at the middle of the block, so that a fade
        // ramp is centred on it rather than lagging a whole block behind. The
        // read position was taken from that same midpoint, which is a different
        // thing entirely: every block then began half a block into the source it
        // was supposed to cover. Constant, so nothing sounded stretched — the
        // sound simply sat a fixed distance from the picture, which at 30fps is
        // 800 samples, about 17 ms.
        //
        // The clip is placed away from the timeline origin and sources from a
        // non-zero in-point, so the assertion is on the real timeline → source
        // mapping rather than on an identity.
        {
            let (start, in_point, len) = (1.0, 3.0, 5.0);
            let p = placed_project(start, in_point, len);
            let n = (in_point + len) as usize * SAMPLE_RATE as usize;
            let mut src = RampSource { len: n };

            // One video frame's worth at 30fps — the block size an export pushes.
            let frames = SAMPLE_RATE as usize / 30;
            // A block one second into the clip, which is source second 4.0.
            let at = start + 1.0;
            let block = mix_block(&p, at, frames, &mut src);
            let (a, b) = span_of(&block, n);

            let want = (in_point + 1.0) * f64::from(SAMPLE_RATE);
            let off = a - want;
            assert!(
                off.abs() < 2.0,
                "a block at timeline {at}s of a clip placed at {start}s from in-point \
             {in_point}s must start at source frame {want}, got {a} — {off:+.0} frames \
             out. Half a block is {:+.0}, which is the read-at-the-midpoint bug.",
                frames as f64 / 2.0,
            );
            // And it covers a block's worth from there, rather than reading the
            // right span at the wrong rate.
            assert!(
                (b - (want + frames as f64 - 1.0)).abs() < 2.0,
                "and ends at source frame {}, got {b}",
                want + frames as f64 - 1.0,
            );
        }
        // Case `consecutive_blocks_abut_with_no_gap_and_no_overlap`:
        // Consecutive blocks have to abut, or the export's audio gains or loses
        // samples every frame and drifts away from the picture over a long
        // timeline.
        {
            let (start, in_point, len) = (1.0, 3.0, 5.0);
            let p = placed_project(start, in_point, len);
            let n = (in_point + len) as usize * SAMPLE_RATE as usize;
            let mut src = RampSource { len: n };

            let frames = SAMPLE_RATE as usize / 30;
            let step = frames as f64 / f64::from(SAMPLE_RATE);
            let mut previous_end: Option<f64> = None;
            for i in 0..30 {
                let at = start + 0.5 + f64::from(i) * step;
                let (a, b) = span_of(&mix_block(&p, at, frames, &mut src), n);
                if let Some(end) = previous_end {
                    assert!(
                        (a - end - 1.0).abs() < 2.0,
                        "block {i} starts at source frame {a}; the one before it ended at \
                     {end}, so the two are {:+.0} frames apart instead of adjacent",
                        a - end - 1.0,
                    );
                }
                previous_end = Some(b);
            }
        }
        // Case `at_normal_speed_a_block_covers_exactly_its_own_frames`.
        {
            let (p, _) = speed_project(1.0, false, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            let block = mix_block(&p, 0.0, 1000, &mut src);
            let (a, b) = span_of(&block, src.len);
            assert!(a.abs() < 1.0, "starts at source frame 0, got {a}");
            assert!((b - 999.0).abs() < 2.0, "and ends at frame 999, got {b}");
        }
        // Case `doubling_the_rate_covers_twice_as_much_source_in_a_block`.
        {
            let (p, _) = speed_project(2.0, false, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            let block = mix_block(&p, 0.0, 1000, &mut src);
            let (a, b) = span_of(&block, src.len);
            assert!(a.abs() < 2.0, "still starts at the in-point, got {a}");
            assert!(
                (b - 1998.0).abs() < 4.0,
                "a 1000-frame block at 2x reads ~2000 source frames, got {b}",
            );
        }
        // Case `halving_the_rate_covers_half_as_much`.
        {
            let (p, _) = speed_project(0.5, false, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            let block = mix_block(&p, 0.0, 1000, &mut src);
            let (a, b) = span_of(&block, src.len);
            assert!(a.abs() < 2.0, "starts at the in-point, got {a}");
            assert!(
                (b - 499.5).abs() < 4.0,
                "a 1000-frame block at 0.5x reads ~500 source frames, got {b}",
            );
        }
        // Case `reversing_walks_the_source_backwards`.
        {
            let (p, _) = speed_project(1.0, true, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            // A block partway in, so there is source on both sides of it.
            let block = mix_block(&p, 2.0, 1000, &mut src);
            let (a, b) = span_of(&block, src.len);
            assert!(
                a > b,
                "a reversed clip's block runs from later source to earlier: {a} → {b}"
            );
            assert!(
                (a - b - 999.0).abs() < 6.0,
                "and covers a block's worth of it: {a} → {b}",
            );
            // A reversed clip starts on the source's *out* point.
            let head = mix_block(&p, 0.0, 64, &mut src);
            let (h, _) = span_of(&head, src.len);
            let end = 10.0 * f64::from(SAMPLE_RATE);
            assert!(
                (h - end).abs() < 200.0,
                "the first timeline frame of a reversed clip is the source's last, got {h}",
            );
        }
        // Case `resampling_interpolates_rather_than_dropping_to_silence`.
        {
            // A slow clip has to invent samples between source frames. Those
            // must lie between their neighbours — a resampler that returned
            // zeroes would show up as a block full of holes.
            let (p, _) = speed_project(0.25, false, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            let block = mix_block(&p, 0.0, 400, &mut src);
            assert!(
                block.iter().all(|s| s.is_finite()),
                "no NaNs or infinities out of the resampler",
            );
            // The ramp must be monotonic: every sample at least as large as the
            // one before it.
            let mono = block
                .chunks(CHANNELS)
                .zip(block.chunks(CHANNELS).skip(1))
                .all(|(a, b)| b[0] >= a[0] - 1e-6);
            assert!(
                mono,
                "a slowed ramp is still a ramp, not a staircase with gaps"
            );
            let zeros = block.iter().filter(|s| **s == 0.0).count();
            assert!(
                zeros < 8,
                "a slowed block is not mostly silence, got {zeros} zero samples"
            );
        }
        // Case `a_speed_change_moves_where_a_later_block_reads_from`.
        {
            // The whole point: the same timeline instant reads different source
            // depending on the rate.
            let mut src = RampSource {
                len: 60 * SAMPLE_RATE as usize,
            };
            let (slow, _) = speed_project(1.0, false, 30.0);
            let (fast, _) = speed_project(3.0, false, 30.0);
            let n = src.len;
            let (a, _) = span_of(&mix_block(&slow, 4.0, 256, &mut src), n);
            let (b, _) = span_of(&mix_block(&fast, 4.0, 256, &mut src), n);
            assert!(
                (b - a * 3.0).abs() < a * 0.02,
                "at 3x, four timeline seconds in is twelve source seconds in: {a} vs {b}",
            );
        }
        // Case `a_keyframed_volume_is_heard_as_a_ramp`.
        {
            let (mut p, _) = speed_project(1.0, false, 10.0);
            let cid = p.clips[0].id;
            {
                let c = p.clip_mut(cid).unwrap();
                c.set_key_at(
                    crate::param::Param::Volume,
                    0.0,
                    0.0,
                    crate::param::Interp::Linear,
                );
                c.set_key_at(
                    crate::param::Param::Volume,
                    8.0,
                    1.0,
                    crate::param::Interp::Linear,
                );
            }
            let plan = |t: f64| plan_audio(&p, t)[0].gain;
            let early = plan(1.0);
            let mid = plan(4.0);
            let late = plan(7.0);
            assert!(
                early < mid && mid < late,
                "the curve rises: {early} {mid} {late}"
            );
            assert!(
                (mid - 0.5).abs() < 0.02,
                "and is linear through the middle: {mid}"
            );
        }
    }

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
                ..Default::default()
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
                gain_db: 0.0,
                pan: 0.0,
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

    // --- monitoring -------------------------------------------------------

    /// Records what the monitor pushed, so the transport can be asserted on
    /// without an audio device.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<f32>>>);

    impl MonitorSink for Recorder {
        fn push(&mut self, block: &[f32]) -> bool {
            self.0.lock().unwrap().extend_from_slice(block);
            true
        }
    }

    /// Waits for `cond`, or gives up. Polling beats a fixed sleep: the
    /// monitor runs on its own thread and the box is shared.
    fn eventually(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn audio_monitor_plays_seeks_follows_edits_and_stops() {
        // Case `a_paused_monitor_makes_no_sound_and_playing_starts_it`.
        {
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let rec = Recorder::default();
            let tap = rec.0.clone();
            let monitor = AudioMonitor::new(
                &p,
                Box::new(rec),
                Box::new(constant(&[(m1, 0.3), (m2, 0.0)])),
                Arc::new(MixMeters::new()),
            );

            assert!(!monitor.is_playing());
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert!(
                tap.lock().unwrap().is_empty(),
                "a paused editor pushes nothing at all — this is the stutter that comes \
             from firing the sink when the transport is not running",
            );

            monitor.play(1.0);
            assert!(
                eventually(|| !tap.lock().unwrap().is_empty()),
                "playing starts feeding the sink",
            );
            assert!(
                eventually(|| tap.lock().unwrap().iter().any(|s| (*s - 0.3).abs() < 1e-6)),
                "and what it feeds is the mixed timeline",
            );

            monitor.pause();
            // Let any block already in flight land, then prove it has stopped.
            std::thread::sleep(std::time::Duration::from_millis(50));
            let settled = tap.lock().unwrap().len();
            std::thread::sleep(std::time::Duration::from_millis(80));
            assert_eq!(
                tap.lock().unwrap().len(),
                settled,
                "pausing stops the monitor rather than letting it run on",
            );
        }
        // Case `the_monitor_advances_by_what_it_mixed`.
        {
            // The audio clock is the sample count, not wall time, so it cannot
            // drift away from the picture.
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let monitor = AudioMonitor::new(
                &p,
                Box::new(Recorder::default()),
                Box::new(constant(&[(m1, 0.1), (m2, 0.1)])),
                Arc::new(MixMeters::new()),
            );
            monitor.play(2.0);
            let block_secs = MONITOR_BLOCK as f64 / f64::from(SAMPLE_RATE);
            assert!(
                eventually(|| monitor.position() >= 2.0 + block_secs * 3.0),
                "the position advances as blocks are mixed, got {}",
                monitor.position(),
            );
            monitor.pause();
            std::thread::sleep(std::time::Duration::from_millis(40));
            let held = monitor.position();
            // A whole number of blocks past where it started.
            let blocks = (held - 2.0) / block_secs;
            assert!(
                (blocks - blocks.round()).abs() < 1e-6,
                "the position is an exact number of mixed blocks past the start: {blocks}",
            );
        }
        // Case `seeking_moves_what_is_heard_next`.
        {
            let (mut p, _, _, _) = project_with_audio();
            // A clip only in the second half of the timeline, so where the
            // monitor is reading from is audible in what it pushes.
            let c2 = p.clips[1].id;
            {
                let c = p.clip_mut(c2).unwrap();
                c.start = 20.0;
                c.out_point = 10.0;
            }
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let rec = Recorder::default();
            let tap = rec.0.clone();
            let monitor = AudioMonitor::new(
                &p,
                Box::new(rec),
                Box::new(constant(&[(m1, 0.2), (m2, 0.9)])),
                Arc::new(MixMeters::new()),
            );

            monitor.play(1.0);
            assert!(eventually(|| !tap.lock().unwrap().is_empty()));
            assert!(
                tap.lock().unwrap().iter().all(|s| *s < 0.5),
                "before the second clip, only the first is heard",
            );

            tap.lock().unwrap().clear();
            monitor.seek(21.0);
            assert!(
                eventually(|| tap.lock().unwrap().iter().any(|s| *s > 0.5)),
                "after seeking into the second clip, it is what plays",
            );
            monitor.pause();
        }
        // Case `an_edit_is_heard_without_restarting_playback`.
        {
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let rec = Recorder::default();
            let tap = rec.0.clone();
            let monitor = AudioMonitor::new(
                &p,
                Box::new(rec),
                Box::new(constant(&[(m1, 0.4), (m2, 0.0)])),
                Arc::new(MixMeters::new()),
            );
            monitor.play(1.0);
            assert!(eventually(|| tap.lock().unwrap().iter().any(|s| *s > 0.3)));

            // Mute the track mid-playback: the very setting that reached no
            // audio path before.
            let mut muted = p;
            muted.tracks[0].muted = true;
            muted.tracks[1].muted = true;
            monitor.set_project(&muted);
            assert!(
                eventually(|| {
                    // Clear, then look at what arrives *after* the clear. A
                    // single clear before looking would race a block that was
                    // already mixed when the mute landed: one pre-mute sample
                    // in the tap and no later silence could ever clear it,
                    // which is a flake rather than a finding.
                    tap.lock().unwrap().clear();
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    let got = tap.lock().unwrap();
                    !got.is_empty() && got.iter().all(|s| *s == 0.0)
                }),
                "muting is heard on the next block, without stopping playback",
            );
            assert!(monitor.is_playing(), "and playback is still running");
            monitor.pause();
        }
        // Case `dropping_the_monitor_stops_its_thread`.
        {
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let rec = Recorder::default();
            let tap = rec.0.clone();
            {
                let monitor = AudioMonitor::new(
                    &p,
                    Box::new(rec),
                    Box::new(constant(&[(m1, 0.5), (m2, 0.0)])),
                    Arc::new(MixMeters::new()),
                );
                monitor.play(1.0);
                assert!(eventually(|| !tap.lock().unwrap().is_empty()));
            }
            // The drop joins the thread, so nothing can arrive after it returns.
            let after = tap.lock().unwrap().len();
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert_eq!(
                tap.lock().unwrap().len(),
                after,
                "a dropped monitor leaves no thread still feeding a sink",
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn mixer_sums_scales_and_silences_by_track_state() {
        // Case `overlapping_clips_sum`.
        {
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
        // Case `clip_volume_scales_what_it_contributes`.
        {
            let (mut p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let c1 = p.clips[0].id;
            p.clip_mut(c1).unwrap().volume = 0.5;
            let mut src = constant(&[(m1, 0.4), (m2, 0.0)]);
            let block = mix_block(&p, 1.0, 16, &mut src);
            assert!(
                (block[0] - 0.2).abs() < 1e-6,
                "half volume halves it: {}",
                block[0]
            );
        }
        // Case `the_fade_envelope_reaches_the_mix`.
        {
            let (mut p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            p.clip_mut(p.clips[0].id).unwrap().fade_in = 4.0;
            let mut src = constant(&[(m1, 1.0), (m2, 0.0)]);
            let head = mix_block(&p, 0.0, 16, &mut src)[0];
            let mid = mix_block(&p, 2.0, 16, &mut src)[0];
            let after = mix_block(&p, 6.0, 16, &mut src)[0];
            assert!(
                head < mid && mid < after,
                "the ramp climbs: {head} {mid} {after}"
            );
            assert!((after - 1.0).abs() < 1e-6, "and reaches full past the fade");
        }
        // Case `a_muted_track_is_silent_and_solo_silences_the_others`.
        {
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
        // Case `a_video_track_never_contributes_to_the_mix`.
        {
            let (mut p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let idx = p
                .tracks
                .iter()
                .position(|t| t.kind == TrackKind::Audio)
                .unwrap();
            p.tracks[idx].kind = TrackKind::Video;
            let mut src = constant(&[(m1, 0.25), (m2, 0.5)]);
            assert!(
                (mix_block(&p, 1.0, 16, &mut src)[0] - 0.5).abs() < 1e-6,
                "only audio lanes are mixed",
            );
        }
        // Case `a_loud_overlap_clamps_instead_of_wrapping`.
        {
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
            assert!(
                first > 32_000,
                "full scale encodes as a large positive: {first}"
            );
        }
        // Case `full_scale_negative_does_not_overflow`.
        {
            let (p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let mut src = constant(&[(m1, -1.0), (m2, 0.0)]);
            let pcm = to_s16le(&mix_block(&p, 1.0, 4, &mut src));
            let v = i16::from_le_bytes([pcm[0], pcm[1]]);
            assert_eq!(
                v, -32767,
                "the negative limit is representable, not wrapped"
            );
        }
    }

    /// Apply a trim to `p` in place, as the `trim-mix` action's document
    /// write does.
    fn apply_lanes(p: &mut Project, lanes: Vec<Track>) {
        for next in lanes {
            if let Some(dst) = p.tracks.iter_mut().find(|t| t.id == next.id) {
                *dst = next;
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn mix_trim_uses_the_measured_overshoot() {
        // Case `trimming_by_what_the_meter_measured_brings_the_real_mix_back_under`:
        // The whole loop, through the production mixer at both ends: a mix goes
        // over, the meter measures how far, the trim comes down by that much and
        // the *same* mixer now fits.
        //
        // Asserted on the peak the mixer really produces rather than on the
        // gains that were written, because "the numbers went down" is not the
        // claim — the claim is that the sum fits.
        {
            let (mut p, a1, _a2, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            // Two lanes that fit on their own and do not fit together, one of them
            // pushed up on its fader.
            p.tracks.iter_mut().find(|t| t.id == a1).unwrap().gain_db = 6.0;
            let mut src = constant(&[(m1, 0.5), (m2, 0.4)]);

            let (_, before) = mix_block_metered(&p, 1.0, 64, &mut src);
            assert!(
                before.master.over(),
                "the fixture has to clip or this proves nothing: {:?}",
                before.master,
            );
            let meters = MixMeters::new();
            meters.record(1.0, &before);
            let snap = meters.snapshot();
            assert!(snap.clipped());

            let trimmed = p.lanes_trimmed_by(&snap.measured_tracks(), snap.worst_over_db());
            apply_lanes(&mut p, trimmed);

            let (_, after) = mix_block_metered(&p, 1.0, 64, &mut src);
            assert!(
                !after.master.over(),
                "the mix the trim was measured from now fits: {:?}",
                after.master,
            );
            // …and it fits by landing *on* full scale, not by being thrown away.
            // A trim that took twenty decibels off would also pass the line above
            // and would have ruined the mix to do it.
            assert!(
                (after.master.loudest() - 1.0).abs() < 0.02,
                "and it lands at full scale rather than somewhere quiet: {}",
                after.master.loudest(),
            );
        }
        // Case `a_trim_moves_the_whole_mix_and_not_the_balance`:
        // The balance survives. Every lane moves by the same number of decibels,
        // so what the person mixing decided about the *relationship* between the
        // lanes is exactly where they left it.
        {
            let (mut p, a1, a2, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            p.tracks.iter_mut().find(|t| t.id == a1).unwrap().gain_db = 6.0;
            p.tracks.iter_mut().find(|t| t.id == a2).unwrap().gain_db = -2.0;
            let mut src = constant(&[(m1, 0.5), (m2, 0.4)]);

            let (_, before) = mix_block_metered(&p, 1.0, 64, &mut src);
            let meters = MixMeters::new();
            meters.record(1.0, &before);
            let snap = meters.snapshot();
            let gap = |p: &Project| p.track(a1).unwrap().gain_db - p.track(a2).unwrap().gain_db;
            let was = gap(&p);
            let trimmed = p.lanes_trimmed_by(&snap.measured_tracks(), snap.worst_over_db());
            apply_lanes(&mut p, trimmed);
            assert!(
                (gap(&p) - was).abs() < 1e-9,
                "the lanes are still {was} dB apart, not {}",
                gap(&p),
            );
            // And both really did move, so the line above is not passing on a
            // trim that did nothing at all.
            assert!(p.track(a1).unwrap().gain_db < 6.0 - 1e-9);
            assert!(p.track(a2).unwrap().gain_db < -2.0 - 1e-9);
        }
        // Case `asking_for_headroom_leaves_headroom`:
        // Headroom is a real request, not a rounding: asking for three decibels
        // under lands three decibels under.
        {
            let (mut p, a1, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            p.tracks.iter_mut().find(|t| t.id == a1).unwrap().gain_db = 6.0;
            let mut src = constant(&[(m1, 0.5), (m2, 0.4)]);
            let (_, before) = mix_block_metered(&p, 1.0, 64, &mut src);
            let meters = MixMeters::new();
            meters.record(1.0, &before);
            let snap = meters.snapshot();

            let trimmed = p.lanes_trimmed_by(&snap.measured_tracks(), snap.worst_over_db() + 3.0);
            apply_lanes(&mut p, trimmed);
            let (_, after) = mix_block_metered(&p, 1.0, 64, &mut src);
            let want = crate::model::db_to_linear(-3.0) as f32;
            assert!(
                (after.master.loudest() - want).abs() < 0.02,
                "asked for 3 dB of headroom and got {} rather than {want}",
                after.master.loudest(),
            );
        }
        // Case `a_mix_can_clip_with_no_single_lane_over_and_the_trim_still_fixes_it`:
        // The case that decides *which* lanes a trim moves.
        //
        // Two lanes each comfortably under full scale on their own add up to a
        // mix that is over. Nothing is in `clipped_tracks` — there is no lane to
        // blame — so a trim that moved only the lanes caught over would have had
        // nothing to move and would have left the mix clipping.
        {
            let (mut p, _, _, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
            let mut src = constant(&[(m1, 0.7), (m2, 0.6)]);

            let (_, before) = mix_block_metered(&p, 1.0, 64, &mut src);
            assert!(before.master.over(), "1.3 is over: {:?}", before.master);
            let meters = MixMeters::new();
            meters.record(1.0, &before);
            let snap = meters.snapshot();
            assert!(
                snap.clipped_tracks().is_empty(),
                "neither lane is over on its own, so neither is to blame",
            );
            assert_eq!(
                snap.measured_tracks().len(),
                2,
                "but both were in the sum, so both are what a trim moves",
            );

            let trimmed = p.lanes_trimmed_by(&snap.measured_tracks(), snap.worst_over_db());
            apply_lanes(&mut p, trimmed);
            let (_, after) = mix_block_metered(&p, 1.0, 64, &mut src);
            assert!(
                !after.master.over(),
                "and it fits afterwards: {:?}",
                after.master,
            );
        }
        // Case `a_trim_of_nothing_writes_nothing`:
        // Nothing to trim writes nothing. A mix that is already under full scale
        // must not be quietened by an action that reports success.
        {
            let (p, _, _, _) = project_with_audio();
            let lanes: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
            assert!(p.lanes_trimmed_by(&lanes, 0.0).is_empty());
            assert!(p.lanes_trimmed_by(&lanes, -3.0).is_empty());
            assert!(p.lanes_trimmed_by(&lanes, f64::NAN).is_empty());
        }
        // Case `only_a_lane_that_can_carry_sound_has_a_level`:
        // Which lanes have a level to show. A meter that can never move is not
        // a quiet meter, it is a broken one.
        {
            let (mut p, a1, _, _) = project_with_audio();
            assert!(
                track_carries_sound(&p, a1),
                "an audio lane is an audio lane whatever is on it",
            );
            // An empty one too: the lane is what it is before anything is dropped
            // on it, and a meter that appeared on the first drop would read as the
            // output coming back to life.
            let empty = Uuid::new_v4();
            p.tracks.push(Track {
                id: empty,
                name: "A3".into(),
                kind: TrackKind::Audio,
                index: 3,
                ..Default::default()
            });
            assert!(track_carries_sound(&p, empty));

            // A picture lane with silent footage on it has nothing to meter…
            let vid = Uuid::new_v4();
            p.tracks.push(Track {
                id: vid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let mid = Uuid::new_v4();
            p.media.push(crate::model::MediaItem {
                id: mid,
                name: "shot.mp4".into(),
                kind: crate::model::MediaKind::Video,
                duration: 10.0,
                has_audio: false,
                ..Default::default()
            });
            p.add_clip(vid, mid, 0.0).expect("media exists");
            assert!(
                !track_carries_sound(&p, vid),
                "silent footage gives its lane no level to show",
            );

            // …and the same lane does the moment the footage carries sound, which
            // is a fact about the file rather than about the track.
            p.media.iter_mut().find(|m| m.id == mid).unwrap().has_audio = true;
            assert!(
                track_carries_sound(&p, vid),
                "a talking head on a video lane is heard, so it is metered",
            );

            // A lane that is not in the project at all has no level either.
            assert!(!track_carries_sound(&p, Uuid::new_v4()));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn linked_video_sound_is_mixed_exactly_once() {
        // Case `a_video_clips_own_sound_is_mixed`:
        // A video file's own sound reaches the mix.
        //
        // The timeline puts a video file on a video track, and the mixer used to
        // read only from audio tracks. So importing a talking-head clip, cutting
        // it and exporting gave back a silent file — with nothing in the preview,
        // the bin or the export dialog admitting it. This is the regression that
        // must never come back.
        {
            use crate::model::{MediaItem, MediaKind, Project, Track, TrackKind};

            let mut p = Project::new("interview");
            let media = uuid::Uuid::new_v4();
            p.media.push(MediaItem {
                id: media,
                name: "interview.mp4".into(),
                kind: MediaKind::Video,
                duration: 30.0,
                // What the container said: a picture that also carries sound.
                has_audio: true,
                ..Default::default()
            });
            let track = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: track,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let clip = p.add_clip(track, media, 0.0).expect("place the clip");
            p.clip_mut(clip).unwrap().out_point = 30.0;

            assert!(
                has_audio(&p),
                "the export asks this before it builds an audio branch at all; \
             answering false is how the sound was dropped from the file",
            );
            let layers = plan_audio(&p, 2.0);
            assert!(
                layers.iter().any(|l| l.clip == clip),
                "the video clip's own sound is not in the mix: {layers:?}",
            );

            // Muting the track the footage sits on silences its sound too — which
            // is what a person means by muting that track.
            p.tracks[0].muted = true;
            assert!(
                plan_audio(&p, 2.0).iter().all(|l| l.clip != clip),
                "muting the video track left its sound playing",
            );
            p.tracks[0].muted = false;

            // A picture whose file carries no sound contributes none, so a silent
            // project does not grow an empty audio branch.
            p.media[0].has_audio = false;
            assert!(
                !has_audio(&p),
                "a video file with no sound track must not claim to carry audio",
            );
            assert!(plan_audio(&p, 2.0).is_empty());
        }
        // Case `a_clip_is_read_at_its_own_source_time`.
        {
            let (mut p, _, _, _) = project_with_audio();
            let c1 = p.clips[0].id;
            {
                let c = p.clip_mut(c1).unwrap();
                c.in_point = 5.0;
                c.out_point = 15.0;
            }
            let layers = plan_audio(&p, 2.0);
            let l = layers
                .iter()
                .find(|l| l.clip == c1)
                .expect("clip is audible");
            assert!(
                (l.source_time - 7.0).abs() < 1e-9,
                "2s into a clip cut at 5s reads 7s of source: {l:?}",
            );
        }
        // Case `a_shot_whose_sound_was_split_off_is_heard_once_not_twice`:
        // Once a shot's sound has a clip of its own, exactly one of the two
        // halves may be mixed. This is the correctness trap of the whole feature:
        // mix both and the shot is heard twice, at double level, and — the moment
        // the two are slid apart — as an echo of itself.
        {
            use crate::model::{MediaItem, MediaKind, Project, Track, TrackKind};

            let mut p = Project::new("interview");
            let media = uuid::Uuid::new_v4();
            p.media.push(MediaItem {
                id: media,
                name: "interview.mp4".into(),
                kind: MediaKind::Video,
                duration: 30.0,
                has_audio: true,
                ..Default::default()
            });
            let v1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: v1,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let a1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: a1,
                name: "A1".into(),
                kind: TrackKind::Audio,
                index: 1,
                ..Default::default()
            });
            let picture = p.add_clip(v1, media, 0.0).expect("place the picture");
            p.clip_mut(picture).expect("picture").out_point = 30.0;

            // Before the split: the picture carries the sound, and is mixed at
            // full level. This is what every project saved before links existed
            // still does, and it must not change.
            let mut src = constant(&[(media, 0.5)]);
            let before = mix_block(&p, 2.0, 8, &mut src);
            assert!(
                (before[0] - 0.5).abs() < 1e-6,
                "a picture that carries sound is mixed: {}",
                before[0],
            );

            let sound = p
                .add_linked_audio(picture, a1)
                .expect("split the sound off");
            let layers = plan_audio(&p, 2.0);
            assert_eq!(
                layers.len(),
                1,
                "exactly one half of a split shot may be mixed: {layers:?}",
            );
            assert_eq!(
                layers[0].clip, sound,
                "and it is the audio clip, which is the one a person can slide,              level and fade",
            );
            let mut src = constant(&[(media, 0.5)]);
            let after = mix_block(&p, 2.0, 8, &mut src);
            assert!(
                (after[0] - 0.5).abs() < 1e-6,
                "splitting the sound off must not change how loud the shot is;              mixing both halves would give 1.0 here, and 0.5 is one of them: {}",
                after[0],
            );
            assert!(has_audio(&p), "the export still finds sound to mux");

            // Levelling the sound half alone is the whole point of it existing.
            p.clip_mut(sound).expect("sound").volume = 0.25;
            let mut src = constant(&[(media, 0.5)]);
            let quiet = mix_block(&p, 2.0, 8, &mut src);
            assert!(
                (quiet[0] - 0.125).abs() < 1e-6,
                "the sound half's own level has to reach the mix: {}",
                quiet[0],
            );
        }
        // Case `unlinking_a_split_shot_does_not_double_it`:
        // Unlinking is explicit and reversible, and it must not make the shot
        // louder. The picture stopped carrying its own sound when the sound was
        // split off, not when the two happened to be linked.
        {
            use crate::model::{MediaItem, MediaKind, Project, Track, TrackKind};

            let mut p = Project::new("interview");
            let media = uuid::Uuid::new_v4();
            p.media.push(MediaItem {
                id: media,
                name: "interview.mp4".into(),
                kind: MediaKind::Video,
                duration: 30.0,
                has_audio: true,
                ..Default::default()
            });
            let v1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: v1,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let a1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: a1,
                name: "A1".into(),
                kind: TrackKind::Audio,
                index: 1,
                ..Default::default()
            });
            let picture = p.add_clip(v1, media, 0.0).expect("place the picture");
            p.clip_mut(picture).expect("picture").out_point = 30.0;
            let sound = p
                .add_linked_audio(picture, a1)
                .expect("split the sound off");

            p.unlink_clips(&[picture, sound]);
            let layers = plan_audio(&p, 2.0);
            assert_eq!(
                layers.len(),
                1,
                "the picture must not start carrying sound the audio clip already              has just because the two were separated: {layers:?}",
            );
            assert_eq!(layers[0].clip, sound);
            let mut src = constant(&[(media, 0.5)]);
            let after = mix_block(&p, 2.0, 8, &mut src);
            assert!(
                (after[0] - 0.5).abs() < 1e-6,
                "still one shot: {}",
                after[0]
            );
        }
        // Case `linking_a_bed_to_a_shot_leaves_the_shots_own_sound_alone`:
        // A link is a general "these move together". Tying a music bed to a shot
        // must not silence the shot.
        {
            use crate::model::{MediaItem, MediaKind, Project, Track, TrackKind};

            let mut p = Project::new("interview");
            let footage = uuid::Uuid::new_v4();
            let music = uuid::Uuid::new_v4();
            for (id, name, kind) in [
                (footage, "interview.mp4", MediaKind::Video),
                (music, "bed.wav", MediaKind::Audio),
            ] {
                p.media.push(MediaItem {
                    id,
                    name: name.into(),
                    kind,
                    duration: 30.0,
                    // The picture's container carries sound; so, obviously, does
                    // the music.
                    has_audio: kind == MediaKind::Video,
                    ..Default::default()
                });
            }
            let v1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: v1,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });
            let a1 = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: a1,
                name: "A1".into(),
                kind: TrackKind::Audio,
                index: 1,
                ..Default::default()
            });
            let picture = p.add_clip(v1, footage, 0.0).expect("place the picture");
            p.clip_mut(picture).expect("picture").out_point = 30.0;
            let bed = p.add_clip(a1, music, 0.0).expect("place the bed");
            p.clip_mut(bed).expect("bed").out_point = 30.0;

            // Tie them together so the bed travels with the shot.
            p.link_clips(&[picture, bed]);
            let layers = plan_audio(&p, 1.0);
            assert!(
                layers.iter().any(|l| l.clip == picture),
                "a link is 'these move together'; only splitting the sound off              moves where the sound comes from: {layers:?}",
            );
            assert!(layers.iter().any(|l| l.clip == bed));
            let mut src = constant(&[(footage, 0.25), (music, 0.5)]);
            let block = mix_block(&p, 1.0, 8, &mut src);
            assert!(
                (block[0] - 0.75).abs() < 1e-6,
                "both are heard: {}",
                block[0],
            );
        }
        // Case `a_video_only_project_reports_no_audio`.
        {
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
                gain_db: 0.0,
                pan: 0.0,
            });
            assert!(!has_audio(&p));
        }
    }
}
