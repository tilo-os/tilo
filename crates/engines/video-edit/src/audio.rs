//! The timeline's audio: which clips are audible at an instant, and what the
//! mixed block is.
//!
//! The mixing is pure — a project and a [`PcmSource`] in, an interleaved block
//! out — so per-clip volume, the fade envelope, track mute and solo, pan and
//! summing overlapping clips are testable without a decoder. It mirrors
//! [`video_picture::preview::plan_layers`]: a muted track contributes nothing, a soloed
//! one silences its unsoloed siblings.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use crate::model::{ClipId, MediaId, Project, TrackId, TrackKind};
use tilo_lock::MutexRecover as _;

/// Everything mixes at one rate, so summing never has to resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono sources are duplicated into both channels by the [`PcmSource`], so the
/// mixer only ever sees interleaved pairs.
pub const CHANNELS: usize = 2;

pub trait PcmSource {
    /// `frames` interleaved stereo frames starting `from_secs` into `media`.
    ///
    /// `None` when the media cannot be read, and the clip then mixes as silence.
    /// A short read past the end of the source is padded by the caller.
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioLayer {
    pub clip: ClipId,
    pub media: MediaId,
    /// The track the meter attributes the level to, so "the mix is clipping" can
    /// be narrowed to "on A1".
    pub track: TrackId,
    pub source_time: f64,
    /// **Clip volume × fade envelope × the track's fader.**
    ///
    /// The first two are a 0..1 trim; the third reaches
    /// [`MAX_GAIN_DB`](crate::model::MAX_GAIN_DB), so this runs `0.0 ..= 4.0` and
    /// a loud layer can be over full scale on its own. That is what the meter is
    /// for.
    pub gain: f64,
    /// `-1`..`1`, applied as [`Track::pan_gains`](crate::model::Track::pan_gains)
    /// when the block is summed, because it is per channel and `gain` is not.
    pub pan: f64,
    /// Signed: source seconds per timeline second, negative when the clip is
    /// reversed.
    pub rate: f64,
}

fn soloing(project: &Project, kind: TrackKind) -> bool {
    project.tracks.iter().any(|t| t.kind == kind && t.solo)
}

/// Whether this clip contributes sound.
///
/// A clip on an audio track always does. So does one on a picture track, carrying
/// its source's own sound (`has_audio` from the container, read at import) —
/// unless that sound has been split off onto an audio clip of its own
/// ([`Clip::sound_split`]), which would otherwise play the shot twice.
///
/// The test asks the picture clip, not the link: unlinking a pair must not double
/// its sound, and linking a music bed to a shot must not silence the shot.
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

/// Mute wins over solo, matching the picture: a muted track stays silent even
/// when it is one of the soloed ones.
pub fn plan_audio(project: &Project, t: f64) -> Vec<AudioLayer> {
    // Solo is asked per track kind, so soloing a music bed does not silence the
    // sound coming off the picture, and vice versa.
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
        // video track silences that clip's sound too.
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
        let level = clip.param_at(crate::param::Param::Volume, t);
        // Volume goes through the animation api, so a keyframed level is just a
        // different number here. Clip volume and envelope are a 0..1 trim and the
        // track's fader scales the result, so no clamp at 1.0: that is what made a
        // quiet clip unfixable.
        let gain = (level * clip.envelope(t)).clamp(0.0, 1.0) * track.gain();
        out.push(AudioLayer {
            clip: clip.id,
            media: clip.media,
            track: track.id,
            source_time: clip.source_time(t),
            gain,
            pan: track.pan,
            rate: clip.speed.signed(),
        });
    }
    out
}

/// An audio track always can; a picture track only when something on it carries its
/// source's own sound.
///
/// A meter that can never move reads as a broken output, so a track without sound
/// gets no meter rather than one at zero.
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

pub fn has_audio(project: &Project) -> bool {
    project.clips.iter().any(|c| {
        project
            .track(c.track)
            .is_some_and(|t| carries_sound(project, c, t.kind))
    })
}

pub fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

pub fn channel_peak(block: &[f32], channel: usize) -> f32 {
    block
        .iter()
        .skip(channel.min(CHANNELS - 1))
        .step_by(CHANNELS)
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// **Before the clamp**, always: after it every overloaded block reads exactly
/// `1.0`, and "just touched full scale" is indistinguishable from "three times
/// over".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlockMeter {
    pub peak: [f32; 2],
}

impl BlockMeter {
    fn measure(block: &[f32]) -> Self {
        Self {
            peak: [channel_peak(block, 0), channel_peak(block, 1)],
        }
    }

    pub fn loudest(&self) -> f32 {
        self.peak[0].max(self.peak[1])
    }

    pub fn over(&self) -> bool {
        self.loudest() > 1.0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixReport {
    pub master: BlockMeter,
    pub tracks: Vec<(TrackId, BlockMeter)>,
}

/// Mix `frames` interleaved stereo frames starting at timeline second `start`.
///
/// Overlapping clips sum, and the sum is clamped to the representable range, so a
/// loud overlap distorts rather than wrapping into noise.
///
/// Gain is evaluated once per block, not per sample: across a 20 ms block a fade
/// moves by well under a decibel.
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

/// [`mix_block`], with what it measured on the way past, taken from the summed
/// block before the clamp — so the meter cannot disagree with what is heard, and
/// nothing is analysed twice.
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

/// Split out so the metered path reads the true peak before it.
fn clamp_block(out: &mut [f32]) {
    for s in out.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
}

/// The summed, **unclamped** block, and each track's own contribution when
/// `per_track` is given. One core, so the plain and the metered mix cannot drift
/// apart about what is audible.
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
    // Gains are read at the middle of the block, so a ramp is centred on it.
    let mid = start + (frames as f64 / f64::from(SAMPLE_RATE)) * 0.5;
    for mut layer in plan_audio(project, mid) {
        if layer.gain <= 0.0 {
            continue;
        }
        // The gain belongs at the block's middle; the read position does not.
        // Planning and reading both at the middle put every block half a block
        // into the source, an offset between sound and picture.
        if let Some(clip) = project.clip(layer.clip) {
            layer.source_time = clip.source_time(start);
        }
        let Some(samples) = fetch_at_rate(source, &layer, frames) else {
            continue;
        };
        // Per channel, so the multiplier alternates; at centre both are the
        // track's plain gain.
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
        // A short read means the source ran out: the rest stays silent rather
        // than repeating the last sample.
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

/// At rate 1.0 this is the plain read. Otherwise it resamples, which changes the
/// pitch along with the tempo — tape speed, not a time-stretch. A half-built
/// phase vocoder would smear transients, which is worse than an honest varispeed.
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
    // One extra frame, so the last output sample has a right-hand neighbour.
    let need = span.ceil() as usize + 2;
    // Reversed, the block covers the source behind the current position, so the
    // read starts there and the samples are walked backwards.
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
        // Reversed, the block covers `(source_time - span, source_time]`, so its
        // first output frame is the last sample before the source time — not the
        // one at it, which for a clip ending at the media's end does not exist.
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
            break;
        };
        let frac = (pos - j as f64) as f32;
        // The window's last sample has no right-hand neighbour. Hold it rather
        // than ramping into a zero the source never had, which would click on the
        // edge of every block that lands there.
        let right = src_frames.get(j + 1).unwrap_or(left);
        for ((o, &a), &b) in out_frame.iter_mut().zip(left).zip(right) {
            *o = a + (b - a) * frac;
        }
    }
    Some(out)
}

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

/// Each source is decoded once, in full, to interleaved stereo `f32` at
/// [`SAMPLE_RATE`] and kept: an export asks for every block of every clip, in
/// order, so a cache holding the whole source never misses twice.
#[cfg(any(feature = "gst", feature = "decode"))]
pub struct DecodedPcm {
    sources: std::collections::HashMap<MediaId, media::Source>,
    loaded: std::collections::HashMap<MediaId, Option<std::sync::Arc<Vec<f32>>>>,
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl DecodedPcm {
    pub fn new(project: &Project) -> Self {
        let sources = project
            .media
            .iter()
            .filter_map(|item| {
                let resolved = item.source();
                resolved
                    .is_decodable()
                    .then(|| (item.id, crate::media::engine_source(&resolved.abs_path)))
            })
            .collect();
        Self {
            sources,
            loaded: std::collections::HashMap::new(),
        }
    }

    fn pcm_for(&mut self, media: MediaId) -> Option<std::sync::Arc<Vec<f32>>> {
        if let Some(hit) = self.loaded.get(&media) {
            return hit.clone();
        }
        let decoded = self
            .sources
            .get(&media)
            .and_then(decode_pcm)
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

/// A source's decoded samples, held between checks: the mix a person re-measures as they edit
/// does not change a source's samples, so a decode answers every later check of the same cut.
/// A file that changed — relinked, replaced, written again — is decoded again. Bounded by
/// bytes, the oldest source leaving when a new one would not fit.
#[cfg(any(feature = "gst", feature = "decode"))]
pub struct SourcePcm {
    entries: Vec<CachedPcm>,
    bytes: usize,
    limit: usize,
}

#[cfg(any(feature = "gst", feature = "decode"))]
struct CachedPcm {
    path: std::path::PathBuf,
    stamp: Stamp,
    pcm: Option<std::sync::Arc<Vec<f32>>>,
}

/// What says a source file is the one that was decoded: its size and the moment it was written.
#[cfg(any(feature = "gst", feature = "decode"))]
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    written: Option<std::time::SystemTime>,
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl Stamp {
    fn of(path: &std::path::Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            written: metadata.modified().ok(),
        })
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl SourcePcm {
    /// A cache holding at most `limit` bytes of decoded samples.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            limit,
        }
    }

    /// The source's samples, decoding them only when the file changed since the last check.
    pub fn pcm(&mut self, source: &media::Source) -> Option<std::sync::Arc<Vec<f32>>> {
        self.pcm_with(source, &mut decode_pcm)
    }

    /// [`Self::pcm`] with the decode `decode` does, so a test can count the decodes the cache
    /// spares.
    fn pcm_with(
        &mut self,
        source: &media::Source,
        decode: &mut dyn FnMut(&media::Source) -> Option<Vec<f32>>,
    ) -> Option<std::sync::Arc<Vec<f32>>> {
        let Some(path) = source.abs_path() else {
            // Nothing on this device says what the source's file is, so there is nothing to
            // keep it by: it is decoded for this check alone.
            return decode(source).map(std::sync::Arc::new);
        };
        let stamp = Stamp::of(&path);
        if let Some(stamp) = &stamp
            && let Some(at) = self.entries.iter().position(|entry| entry.path == path)
        {
            let entry = self.entries.remove(at);
            if entry.stamp == *stamp {
                let pcm = entry.pcm.clone();
                // The source just asked for stands last in line to leave.
                self.entries.push(entry);
                return pcm;
            }
            // The file changed under the cut: what was decoded of it is no longer its samples.
            self.bytes = self.bytes.saturating_sub(bytes_of(entry.pcm.as_ref()));
        }
        let pcm = decode(source).map(std::sync::Arc::new);
        let Some(stamp) = stamp else {
            // Nothing says what the file is, so nothing says the decode still holds: it answers
            // this check and is not kept.
            return pcm;
        };
        let bytes = bytes_of(pcm.as_ref());
        if bytes > self.limit {
            return pcm;
        }
        self.entries.push(CachedPcm {
            path,
            stamp,
            pcm: pcm.clone(),
        });
        self.bytes = self.bytes.saturating_add(bytes);
        while self.bytes > self.limit && self.entries.len() > 1 {
            let gone = self.entries.remove(0);
            self.bytes = self.bytes.saturating_sub(bytes_of(gone.pcm.as_ref()));
        }
        pcm
    }

    /// How many sources it holds decoded.
    #[must_use]
    pub fn held(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
fn bytes_of(pcm: Option<&std::sync::Arc<Vec<f32>>>) -> usize {
    pcm.map_or(0, |pcm| pcm.len() * std::mem::size_of::<f32>())
}

/// A mix that reads its sources through [`SourcePcm`]: the same samples as [`DecodedPcm`],
/// decoded once across checks instead of once per check.
#[cfg(any(feature = "gst", feature = "decode"))]
pub struct CachedSource<'a> {
    sources: std::collections::HashMap<MediaId, media::Source>,
    cache: &'a mut SourcePcm,
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl<'a> CachedSource<'a> {
    pub fn new(project: &Project, cache: &'a mut SourcePcm) -> Self {
        let sources = project
            .media
            .iter()
            .filter_map(|item| {
                let resolved = item.source();
                resolved
                    .is_decodable()
                    .then(|| (item.id, crate::media::engine_source(&resolved.abs_path)))
            })
            .collect();
        Self { sources, cache }
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
impl PcmSource for CachedSource<'_> {
    fn samples(&mut self, media: MediaId, from_secs: f64, frames: usize) -> Option<Vec<f32>> {
        let source = self.sources.get(&media)?;
        let pcm = self.cache.pcm(source)?;
        let start = (from_secs.max(0.0) * f64::from(SAMPLE_RATE)) as usize * CHANNELS;
        if start >= pcm.len() {
            return None;
        }
        let end = (start + frames * CHANNELS).min(pcm.len());
        pcm.get(start..end).map(<[f32]>::to_vec)
    }
}

/// `None` when the source yields no audio at all: a silent video is not an error.
/// A decode that stalls past its budget mixes as silence rather than freezing an
/// export.
#[cfg(any(feature = "gst", feature = "decode"))]
pub fn decode_pcm(source: &media::Source) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    let end = media::pcm::decode(
        source,
        SAMPLE_RATE,
        CHANNELS as u32,
        std::time::Duration::from_secs(120),
        |samples| out.extend_from_slice(samples),
    );
    if end == media::pcm::PcmEnd::TimedOut {
        tracing::warn!("audio: decoding {source:?} timed out; mixing it as silence");
    }
    (!out.is_empty()).then_some(out)
}

/// A peak meter rises instantly and falls slowly: the rise is the event and the
/// fall only makes the next one legible. 24 dB/s sits between the broadcast PPM
/// and a DAW's near-instant fall.
const METER_RELEASE_DB_PER_SEC: f64 = 24.0;

const OVER_MERGE_SECS: f64 = 0.5;

/// Past this the list stops growing rather than dropping the oldest: the *first*
/// place the mix went over is the one to fix.
const MAX_OVERS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackLevel {
    pub track: TrackId,
    pub level: [f32; 2],
    pub hold: f32,
    pub clipped: bool,
}

/// Taken once per `sync` and read from plain fields during `draw`, so the paint
/// path never takes the mixing thread's lock and never allocates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MixSnapshot {
    pub master: [f32; 2],
    pub master_hold: f32,
    pub tracks: Vec<TrackLevel>,
    pub worst: f32,
    pub overs: Vec<f64>,
    pub monitored: bool,
}

impl MixSnapshot {
    pub fn clipped(&self) -> bool {
        !self.overs.is_empty()
    }

    pub fn worst_over_db(&self) -> f64 {
        if self.worst <= 1.0 {
            return 0.0;
        }
        20.0 * f64::from(self.worst).log10()
    }

    pub fn hold_db(&self) -> Option<f64> {
        self.monitored
            .then(|| crate::model::linear_to_db(f64::from(self.master_hold)))
    }

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

    /// The tracks to name in a warning are the ones caught over; the tracks to move
    /// are all of them, because the mix is their sum. See
    /// [`Project::tracks_trimmed_by`](crate::model::Project::tracks_trimmed_by).
    pub fn measured_tracks(&self) -> Vec<TrackId> {
        self.tracks.iter().map(|t| t.track).collect()
    }

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

#[derive(Default)]
pub struct MixMeters {
    state: Mutex<MeterState>,
}

impl MixMeters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Levels only ever rise here; the fall is [`Self::decay`]'s job, driven by the
    /// frame clock, so the fall rate is a property of time rather than of how often
    /// the mixer ran.
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
            let Some(t) = s.tracks.get_mut(i) else {
                continue;
            };
            for (level, peak) in t.level.iter_mut().zip(m.peak) {
                *level = level.max(peak);
            }
            t.hold = t.hold.max(m.loudest());
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

    /// Called when the mix itself changes (see [`mix_signature`]): a warning about
    /// levels the user has since altered describes a mix that no longer exists.
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

/// Tells "the warning still describes this timeline" from "the levels changed".
/// Deliberately narrow: renaming a track must not throw a clipping report away,
/// and moving an audible clip must.
pub fn mix_signature(project: &Project) -> u64 {
    // FNV-1a: stable across runs and machines; a change detector, not a security
    // boundary.
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

pub const MONITOR_BLOCK: usize = 1024;

pub trait MonitorSink: Send {
    fn push(&mut self, block: &[f32]) -> bool;
}

/// What plays on a machine with no audio output: playback still runs, silently.
pub struct NullSink;

impl MonitorSink for NullSink {
    fn push(&mut self, _block: &[f32]) -> bool {
        true
    }
}

#[cfg(any(feature = "gst", feature = "decode"))]
pub fn open_output() -> Box<dyn MonitorSink> {
    if let Some(s) = media::pcm::Playback::open() {
        Box::new(s)
    } else {
        tracing::info!("no audio output available; monitoring silently");
        Box::new(NullSink)
    }
}

struct MonitorState {
    project: Mutex<Project>,
    position: Mutex<f64>,
    playing: AtomicBool,
    stop: AtomicBool,
    #[cfg(test)]
    paused_turns: std::sync::atomic::AtomicUsize,
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
            #[cfg(test)]
            paused_turns: std::sync::atomic::AtomicUsize::new(0),
        });
        let state = shared.clone();
        let handle = tilo_rt::spawn_thread("video-editor-monitor", move || {
            let block_secs = MONITOR_BLOCK as f64 / f64::from(SAMPLE_RATE);
            while !state.stop.load(AtomicOrdering::Relaxed) {
                if !state.playing.load(AtomicOrdering::Relaxed) {
                    #[cfg(test)]
                    state.paused_turns.fetch_add(1, AtomicOrdering::SeqCst);
                    let _ = tilo_rt::sleep_thread(std::time::Duration::from_millis(10));
                    continue;
                }
                let at = *state.position.lock_recover();
                let (block, report) = {
                    let project = state.project.lock_recover();
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
                *state.position.lock_recover() = at + block_secs;
            }
        })
        .ok();
        Self { shared, handle }
    }

    pub fn set_project(&self, project: &Project) {
        if let Ok(mut p) = self.shared.project.lock() {
            *p = project.clone();
        }
    }

    pub fn play(&self, secs: f64) {
        self.seek(secs);
        self.shared.playing.store(true, AtomicOrdering::Relaxed);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, AtomicOrdering::Relaxed);
    }

    pub fn seek(&self, secs: f64) {
        if let Ok(mut p) = self.shared.position.lock() {
            *p = secs.max(0.0);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(AtomicOrdering::Relaxed)
    }

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

// The host's output takes the mix as it is made, with no conversion.
const _: () = assert!(
    SAMPLE_RATE == media::audio_output::RATE && CHANNELS == media::audio_output::CHANNELS as usize
);

#[cfg(any(feature = "gst", feature = "decode"))]
impl MonitorSink for media::pcm::Playback {
    fn push(&mut self, block: &[f32]) -> bool {
        media::pcm::Playback::push(self, block)
    }
}

#[cfg(all(test, any(feature = "gst", feature = "decode")))]
mod pcm_cache_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::SourcePcm;

    #[test]
    fn a_source_is_decoded_once_until_the_file_changes() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let path = dir.path().join("take.wav");
        std::fs::write(&path, b"first take").expect("the source");
        let source = crate::media::engine_source(&path);
        let mut cache = SourcePcm::new(1 << 20);
        let decodes = AtomicUsize::new(0);
        let mut decode = |_: &media::Source| {
            decodes.fetch_add(1, Ordering::Relaxed);
            Some(vec![0.25_f32; 8])
        };
        let first = cache.pcm_with(&source, &mut decode).expect("samples");
        let again = cache.pcm_with(&source, &mut decode).expect("samples");
        assert!(
            Arc::ptr_eq(&first, &again),
            "the held samples answer the second check"
        );
        assert_eq!(decodes.load(Ordering::Relaxed), 1, "a source decodes once");
        // The file changed under the cut: it is decoded again, and the old samples go.
        std::fs::write(&path, b"a longer second take").expect("the source again");
        let third = cache.pcm_with(&source, &mut decode).expect("samples");
        assert_eq!(
            decodes.load(Ordering::Relaxed),
            2,
            "a changed file decodes again"
        );
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(cache.held(), 1, "one source, one set of samples");
    }
}
