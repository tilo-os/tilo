//! The timeline's audio: which clips are audible at an instant, and what the
//! mixed block is.
//!
//! The mixing is pure — a project and a [`PcmSource`] in, an interleaved block
//! out — so per-clip volume, the fade envelope, track mute and solo, pan and
//! summing overlapping clips are testable without a decoder. It mirrors
//! [`crate::preview::plan_layers`]: a muted track contributes nothing, a soloed
//! one silences its unsoloed siblings.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use crate::model::{ClipId, MediaId, Project, TrackId, TrackKind};
use ui::lock::MutexRecover as _;

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
    /// The lane the meter attributes the level to, so "the mix is clipping" can
    /// be narrowed to "on A1".
    pub track: TrackId,
    pub source_time: f64,
    /// **Clip volume × fade envelope × the lane's fader.**
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
/// A clip on an audio lane always does. So does one on a picture lane, carrying
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
        // Volume goes through the animation door, so a keyframed level is just a
        // different number here. Clip volume and envelope are a 0..1 trim and the
        // lane's fader scales the result, so no clamp at 1.0: that is what made a
        // quiet clip unfixable.
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

/// An audio lane always can; a picture lane only when something on it carries its
/// source's own sound.
///
/// A meter that can never move reads as a broken output, so a lane without sound
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

/// The summed, **unclamped** block, and each lane's own contribution when
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
        // lane's plain gain.
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

/// `None` when the source yields no audio at all: a silent video is not an error.
/// A decode that stalls past its budget mixes as silence rather than freezing an
/// export.
#[cfg(any(feature = "gst", feature = "decode"))]
pub(crate) fn decode_pcm(source: &media::Source) -> Option<Vec<f32>> {
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

/// Without it a second of sustained clipping is 47 entries, and a count that says
/// "47 places" about one place.
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

    /// The lanes to name in a warning are the ones caught over; the lanes to move
    /// are all of them, because the mix is their sum. See
    /// [`Project::lanes_trimmed_by`](crate::model::Project::lanes_trimmed_by).
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

/// Shared between the thread that mixes and the one that draws. Nothing here
/// re-analyses anything: the mixer hands over what it already computed.
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
    /// Returning `false` stops the monitor: the device went away.
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
    /// Where the next block is mixed from, in timeline seconds.
    position: Mutex<f64>,
    playing: AtomicBool,
    stop: AtomicBool,
    /// Turns the monitor thread took through its paused branch.
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
        let handle = ui::rt::spawn_thread("video-editor-monitor", move || {
            let block_secs = MONITOR_BLOCK as f64 / f64::from(SAMPLE_RATE);
            while !state.stop.load(AtomicOrdering::Relaxed) {
                if !state.playing.load(AtomicOrdering::Relaxed) {
                    #[cfg(test)]
                    state.paused_turns.fetch_add(1, AtomicOrdering::SeqCst);
                    let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(10));
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

    /// Start playing from `secs`.
    pub fn play(&self, secs: f64) {
        self.seek(secs);
        self.shared.playing.store(true, AtomicOrdering::Relaxed);
    }

    /// Stop playing, leaving the position where it is.
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

    /// Where the monitor has mixed up to, in timeline seconds.
    pub fn position(&self) -> f64 {
        self.shared.position.lock().map_or(0.0, |p| *p)
    }

    #[cfg(test)]
    fn wait_paused(&self) {
        let seen = self.shared.paused_turns.load(AtomicOrdering::SeqCst);
        tilo_test_env::wait_until("the monitor thread sees the pause", || {
            self.shared.paused_turns.load(AtomicOrdering::SeqCst) > seen
        });
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

/// A [`MonitorSink`] that plays through the host's audio output.
#[cfg(any(feature = "gst", feature = "decode"))]
impl MonitorSink for media::pcm::Playback {
    fn push(&mut self, block: &[f32]) -> bool {
        media::pcm::Playback::push(self, block)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "decode")]
    mod media_stack {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/media_stack.rs"
        ));
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "silence is written as literal zeros, not computed"
    )]
    fn mixer_fills_missing_or_short_sources_with_silence() {
        {
            // An un-hydrated reference must not fail the mix: the rest is still audible.
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
        {
            struct Short;
            impl PcmSource for Short {
                fn samples(&mut self, _m: MediaId, _f: f64, _frames: usize) -> Option<Vec<f32>> {
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
    }

    /// A file in the repository, so this test cannot pass by looking at nothing.
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
    fn a_source_that_carries_audio_decodes_to_real_samples() {
        if !media_stack::media_stack() {
            return;
        }
        let source = crate::media::engine_source(&sample("Ambient_Pad.wav"));
        let pcm = super::decode_pcm(&source).expect("the fixture has an audio stream");
        assert!(
            pcm.len() > super::SAMPLE_RATE as usize / 4,
            "a real read, not a handful of frames: {} samples",
            pcm.len(),
        );
        assert!(super::peak(&pcm) > 0.01, "and it is not silence");
    }

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
                let v = n as f32 / self.len as f32;
                out.push(v);
                out.push(v);
            }
            if out.is_empty() { None } else { Some(out) }
        }
    }

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

    fn span_of(block: &[f32], len: usize) -> (f64, f64) {
        let first = f64::from(block[0]) * len as f64;
        let last = f64::from(block[block.len() - CHANNELS]) * len as f64;
        (first, last)
    }

    /// Both offsets are non-zero on purpose: at zero the timeline second and the
    /// source second are the same number, so a mixer that confuses them looks
    /// right.
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
        // A block must read the span of source it *covers*, not one displaced from
        // it: the clip sits away from the timeline origin and sources from a
        // non-zero in-point, so this asserts the real timeline to source map.
        {
            let (start, in_point, len) = (1.0, 3.0, 5.0);
            let p = placed_project(start, in_point, len);
            let n = (in_point + len) as usize * SAMPLE_RATE as usize;
            let mut src = RampSource { len: n };

            let frames = SAMPLE_RATE as usize / 30;
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
            assert!(
                (b - (want + frames as f64 - 1.0)).abs() < 2.0,
                "and ends at source frame {}, got {b}",
                want + frames as f64 - 1.0,
            );
        }
        // Consecutive blocks have to abut, or the export gains or loses samples
        // every frame and drifts away from the picture.
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
        {
            let (p, _) = speed_project(1.0, true, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
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
            let head = mix_block(&p, 0.0, 64, &mut src);
            let (h, _) = span_of(&head, src.len);
            let end = 10.0 * f64::from(SAMPLE_RATE);
            assert!(
                (h - end).abs() < 200.0,
                "the first timeline frame of a reversed clip is the source's last, got {h}",
            );
        }
        {
            // Invented samples must lie between their neighbours: zeroes would
            // show as a block full of holes.
            let (p, _) = speed_project(0.25, false, 10.0);
            let mut src = RampSource {
                len: 10 * SAMPLE_RATE as usize,
            };
            let block = mix_block(&p, 0.0, 400, &mut src);
            assert!(
                block.iter().all(|s| s.is_finite()),
                "no NaNs or infinities out of the resampler",
            );
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
        {
            // The same timeline instant reads different source at different rates.
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

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<f32>>>);

    impl MonitorSink for Recorder {
        fn push(&mut self, block: &[f32]) -> bool {
            self.0.lock().unwrap().extend_from_slice(block);
            true
        }
    }

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
            monitor.wait_paused();
            assert!(
                tap.lock().unwrap().is_empty(),
                "a paused editor pushes nothing at all — this is the stutter that comes \
             from firing the sink when the transport is not running",
            );

            monitor.play(1.0);
            tilo_test_env::wait_until("playing starts feeding the sink", || {
                !tap.lock().unwrap().is_empty()
            });
            tilo_test_env::wait_until("the mixed timeline in the sink", || {
                tap.lock().unwrap().iter().any(|s| (*s - 0.3).abs() < 1e-6)
            });

            monitor.pause();
            monitor.wait_paused();
            let settled = tap.lock().unwrap().len();
            monitor.wait_paused();
            assert_eq!(
                tap.lock().unwrap().len(),
                settled,
                "pausing stops the monitor rather than letting it run on",
            );
        }
        {
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
            monitor.wait_paused();
            let held = monitor.position();
            let blocks = (held - 2.0) / block_secs;
            assert!(
                (blocks - blocks.round()).abs() < 1e-6,
                "the position is an exact number of mixed blocks past the start: {blocks}",
            );
        }
        {
            let (mut p, _, _, _) = project_with_audio();
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
            tilo_test_env::wait_until("the sink to be fed", || !tap.lock().unwrap().is_empty());
            assert!(
                tap.lock().unwrap().iter().all(|s| *s < 0.5),
                "before the second clip, only the first is heard",
            );

            tap.lock().unwrap().clear();
            monitor.seek(21.0);
            tilo_test_env::wait_until(
                "after seeking into the second clip, it is what plays",
                || tap.lock().unwrap().iter().any(|s| *s > 0.5),
            );
            monitor.pause();
        }
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
            tilo_test_env::wait_until("the first clip to be heard", || {
                tap.lock().unwrap().iter().any(|s| *s > 0.3)
            });

            let mut muted = p;
            muted.tracks[0].muted = true;
            muted.tracks[1].muted = true;
            monitor.set_project(&muted);
            tilo_test_env::wait_until("muting to be heard on the next block", || {
                // Clear, then look at what arrives after the clear: one pre-mute
                // block already in the tap would otherwise make this a flake.
                tap.lock().unwrap().clear();
                tilo_test_env::wait_until("a block after the clear", || {
                    !tap.lock().unwrap().is_empty()
                });
                let got = tap.lock().unwrap();
                !got.is_empty() && got.iter().all(|s| *s == 0.0)
            });
            assert!(monitor.is_playing(), "and playback is still running");
            monitor.pause();
        }
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
                tilo_test_env::wait_until("the sink to be fed", || !tap.lock().unwrap().is_empty());
            }
            assert_eq!(
                Arc::strong_count(&tap),
                1,
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

            p.tracks[i2].solo = true;
            p.tracks[i2].muted = true;
            assert!(
                (mix_block(&p, 1.0, 16, &mut src)[0] - 0.25).abs() < 1e-6,
                "a muted track stays silent even when soloed",
            );
        }
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
        {
            // A plain cast would wrap +1.2 round to a large negative sample: an
            // audible crack at exactly the loudest moment.
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
        // The whole loop through the production mixer at both ends, asserted on the
        // peak it really produces: "the numbers went down" is not the claim, the
        // claim is that the sum fits.
        {
            let (mut p, a1, _a2, _) = project_with_audio();
            let (m1, m2) = (p.media[0].id, p.media[1].id);
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
            assert!(
                (after.master.loudest() - 1.0).abs() < 0.02,
                "and it lands at full scale rather than somewhere quiet: {}",
                after.master.loudest(),
            );
        }
        // The balance survives: every lane moves by the same number of decibels.
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
            assert!(p.track(a1).unwrap().gain_db < 6.0 - 1e-9);
            assert!(p.track(a2).unwrap().gain_db < -2.0 - 1e-9);
        }
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
        // Two lanes each under full scale can add up to a mix that is over, and no
        // lane is in `clipped_tracks` to blame; a trim that moved only those would
        // leave the mix clipping.
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
        {
            let (p, _, _, _) = project_with_audio();
            let lanes: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
            assert!(p.lanes_trimmed_by(&lanes, 0.0).is_empty());
            assert!(p.lanes_trimmed_by(&lanes, -3.0).is_empty());
            assert!(p.lanes_trimmed_by(&lanes, f64::NAN).is_empty());
        }
        // A meter that can never move is not a quiet meter, it is a broken one.
        {
            let (mut p, a1, _, _) = project_with_audio();
            assert!(
                track_carries_sound(&p, a1),
                "an audio lane is an audio lane whatever is on it",
            );
            let empty = Uuid::new_v4();
            p.tracks.push(Track {
                id: empty,
                name: "A3".into(),
                kind: TrackKind::Audio,
                index: 3,
                ..Default::default()
            });
            assert!(track_carries_sound(&p, empty));

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

            p.media.iter_mut().find(|m| m.id == mid).unwrap().has_audio = true;
            assert!(
                track_carries_sound(&p, vid),
                "a talking head on a video lane is heard, so it is metered",
            );

            assert!(!track_carries_sound(&p, Uuid::new_v4()));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn linked_video_sound_is_mixed_exactly_once() {
        // A video file's own sound reaches the mix: the timeline puts it on a video
        // track, and a mixer reading only audio tracks exported silence.
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

            p.tracks[0].muted = true;
            assert!(
                plan_audio(&p, 2.0).iter().all(|l| l.clip != clip),
                "muting the video track left its sound playing",
            );
            p.tracks[0].muted = false;

            p.media[0].has_audio = false;
            assert!(
                !has_audio(&p),
                "a video file with no sound track must not claim to carry audio",
            );
            assert!(plan_audio(&p, 2.0).is_empty());
        }
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
        // Once a shot's sound has a clip of its own, exactly one of the two halves
        // may be mixed: both would play it twice, and as an echo once slid apart.
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
